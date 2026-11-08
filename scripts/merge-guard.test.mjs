// Tests for .claude/hooks/merge-guard.mjs, the merge guard (ADR-0023). Run with `just scripts-test`.
//
// Every git and gh call goes through an injected `run`, and every file read through an injected
// `readFile`, so no test touches this repository's git state, its target/review/ or the network.
// A call the scenario doesn't expect throws, which the guard turns into a refusal. The
// command-line tests feed the real hook only commands it refuses or passes before any call. The
// settings' tests pin .claude/settings.json, which runs the hook and holds the permission rules.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { decide, dependencyLines, SENSITIVE, scan } from "../.claude/hooks/merge-guard.mjs";
import { GATES } from "./coverage-gates.mjs";

const HOOK = fileURLToPath(new URL("../.claude/hooks/merge-guard.mjs", import.meta.url));
const SETTINGS = fileURLToPath(new URL("../.claude/settings.json", import.meta.url));

const PROJECT = "C:/path/to/afkfleet";
const BRANCH = "p13/p13.1-example";
const SLUG = "p13-p13.1-example";
const SHA = "0123456789abcdef0123456789abcdef01234567";
const OTHER = "89abcdef0123456789abcdef0123456789abcdef";
const BASE = "fedcba9876543210fedcba9876543210fedcba98";
const PR = "101";
const MERGE = `gh pr merge ${PR} --squash --match-head-commit ${SHA} --delete-branch`;
const VERDICT = path.join(PROJECT, "target", "review", SLUG, "verdict.json");

const TOPLEVEL = "git rev-parse --show-toplevel";
const SYMBOLIC = "git symbolic-ref --quiet --short HEAD";
const HEAD = "git rev-parse HEAD";
const DIFF = "git diff --name-status --no-renames -z origin/main...HEAD";
const MERGE_BASE = "git merge-base origin/main HEAD";
const LS_REMOTE = `git ls-remote origin refs/heads/${BRANCH}`;
const VIEW = `gh pr view ${PR} --json headRefName,headRefOid,baseRefName,state,isCrossRepository`;
const CHECKS = `gh pr checks ${PR} --json name,bucket`;

/** A call's reply with `stdout` and exit 0. */
function ok(stdout) {
  return { status: 0, stdout, stderr: "" };
}

/** A call's reply with exit `status` and `stdout`. */
function exited(status, stdout = "") {
  return { status, stdout, stderr: "" };
}

/** `git diff --name-status -z`'s output for `status, path` pairs. */
function z(...fields) {
  return fields.map((field) => `${field}\0`).join("");
}

/** The hook's input for `command` from the main session, with `extra` fields. */
function input(command, extra = {}) {
  return {
    session_id: "s1",
    hook_event_name: "PreToolUse",
    tool_name: "Bash",
    tool_input: { command, description: "test" },
    cwd: PROJECT,
    ...extra,
  };
}

/** The PR as `gh pr view` answers it, with `change` applied. */
function view(change = {}) {
  return ok(
    JSON.stringify({
      headRefName: BRANCH,
      headRefOid: SHA,
      baseRefName: "main",
      state: "OPEN",
      isCrossRepository: false,
      ...change,
    }),
  );
}

/** The verdict file's text, with `change` applied. */
function verdict(change = {}) {
  return JSON.stringify({ head: SHA, verdict: "APPROVE", round: 2, findings: [], ...change });
}

/**
 * A world in which `MERGE` is allowed: a pushed, approved, green PR that touches no sensitive
 * path. Tests change its `replies` (keyed by the call's words) and `files` before deciding.
 */
function world() {
  const calls = [];
  const replies = new Map([
    [TOPLEVEL, ok(`${PROJECT}\n`)],
    [SYMBOLIC, ok(`${BRANCH}\n`)],
    [HEAD, ok(`${SHA}\n`)],
    [DIFF, ok(z("M", "crates/fleet-core/src/bot.rs", "A", "docs/adr/0024-example.md"))],
    [MERGE_BASE, ok(`${BASE}\n`)],
    [LS_REMOTE, ok(`${SHA}\trefs/heads/${BRANCH}\n`)],
    [VIEW, view()],
    [
      CHECKS,
      ok(
        JSON.stringify([
          { name: "fmt", bucket: "pass" },
          { name: "e2e", bucket: "pass" },
        ]),
      ),
    ],
  ]);
  const files = new Map([[VERDICT, verdict()]]);
  const deps = {
    projectDir: PROJECT,
    run(command, args) {
      const key = [command, ...args].join(" ");
      calls.push(key);
      const reply = replies.get(key);
      if (reply === undefined) {
        throw new Error(`unexpected call: ${key}`);
      }
      return reply;
    },
    readFile(file) {
      const text = files.get(file);
      if (text === undefined) {
        throw Object.assign(new Error(`ENOENT: ${file}`), { code: "ENOENT" });
      }
      return text;
    },
  };
  return { calls, replies, files, deps };
}

/** Asserts that `result` refuses with a reason matching `pattern`. */
function refused(result, pattern) {
  assert.equal(result.allow, false, `expected a refusal, got ${JSON.stringify(result)}`);
  assert.match(result.reason, pattern);
}

/** Decides `command` in a fresh allowed world after `change(world)`. */
function decideIn(change, command = MERGE, extra = {}) {
  const w = world();
  change(w);
  return { result: decide(input(command, extra), w.deps), calls: w.calls };
}

describe("commands that aren't merges", () => {
  it("a command that isn't a merge, push or PR passes without a single call", () => {
    const w = world();

    const result = decide(input("cargo nextest run -p fleet-core"), w.deps);

    assert.deepEqual(result, { allow: true });
    assert.deepEqual(w.calls, []);
  });

  it("the main session's push and PR creation pass without a call: the permission rules judge them", () => {
    for (const command of ["git push -u origin p13/p13.1-example", "gh pr create --fill"]) {
      const w = world();

      assert.deepEqual(decide(input(command), w.deps), { allow: true });
      assert.deepEqual(w.calls, []);
    }
  });

  it("a tool that isn't a shell passes", () => {
    const w = world();

    const result = decide({ ...input(MERGE), tool_name: "Read" }, w.deps);

    assert.deepEqual(result, { allow: true });
  });

  it("a shell call without a command is refused", () => {
    const w = world();

    refused(decide({ ...input(MERGE), tool_input: {} }, w.deps), /without a command/);
  });
});

describe("finding a merge", () => {
  for (const [name, command] of [
    ["plainly", MERGE],
    ["after &&", `cd crates && ${MERGE}`],
    ["inside bash -c", `bash -c "${MERGE}"`],
    ["with quoted words", `gh "pr" 'merge' ${PR}`],
    ["with tabs and runs of spaces", `gh\tpr   merge ${PR}`],
    ["after PowerShell's call operator", `& gh pr merge ${PR}`],
    ["through a path to gh.exe", `"C:\\Program Files\\GitHub CLI\\gh.exe" pr merge ${PR}`],
    ["in upper case", `GH PR MERGE ${PR}`],
    ["with a backslash escape", `g\\h pr merge ${PR}`],
    ["with PowerShell's backtick escape", `gh pr mer\`ge ${PR}`],
    ["inside a substitution", `x=$(gh pr merge ${PR})`],
    ["with a flag before merge", `gh pr -R owner/repo merge ${PR}`],
    ["echoed", "echo gh pr merge"],
  ]) {
    it(`finds a merge ${name}`, () => {
      assert.equal(scan(command).merge, true);
    });
  }

  for (const [name, command] of [
    ["gh pr view", `gh pr view ${PR} --json state`],
    ["merge in the next command", `gh pr view ${PR} && echo merge`],
    ["git merge", "git merge main"],
    ["the words in another order", "echo pr merge gh"],
  ]) {
    it(`finds no merge in ${name}`, () => {
      assert.equal(scan(command).merge, false);
    });
  }

  it("finds pushes and PR creation, and not a push in a grep", () => {
    assert.equal(scan("git push -u origin p13/x").push, true);
    assert.equal(scan("git -C . push origin p13/x").push, true);
    assert.equal(scan("git log --grep=push").push, false);
    assert.equal(scan("gh pr create --fill").create, true);
    assert.equal(scan("gh pr view 5").create, false);
  });
});

describe("subagents (ADR-0023, U2)", () => {
  for (const command of [
    "git push -u origin p13/x",
    "git -C . push origin p13/x",
    "gh pr create --fill",
    MERGE,
  ]) {
    it(`a subagent's ${command.split(" ").slice(0, 3).join(" ")} is refused without a call`, () => {
      const { result, calls } = decideIn(() => {}, command, {
        agent_id: "a1",
        agent_type: "pr-reviewer",
      });

      refused(result, /only the main session pushes, opens pull requests and merges/);
      assert.deepEqual(calls, []);
    });
  }

  it("a subagent's read-only command passes", () => {
    const { result } = decideIn(() => {}, "git diff origin/main...HEAD", { agent_id: "a1" });

    assert.deepEqual(result, { allow: true });
  });
});

describe("the command's form (D1)", () => {
  const plainly = /alone and written plainly/;
  for (const [name, command, pattern] of [
    ["--admin", `${MERGE} --admin`, /--admin is never used/],
    ["--auto", `${MERGE} --auto`, /--auto is never used/],
    ["no --squash", `gh pr merge ${PR} --match-head-commit ${SHA}`, /--squash is required/],
    ["--merge", `gh pr merge ${PR} --merge --squash --match-head-commit ${SHA}`, /"--merge"/],
    ["no --match-head-commit", `gh pr merge ${PR} --squash`, /40-character SHA/],
    [
      "a short SHA",
      `gh pr merge ${PR} --squash --match-head-commit ${SHA.slice(0, 7)}`,
      /40-character SHA/,
    ],
    [
      "a SHA in upper case",
      `gh pr merge ${PR} --squash --match-head-commit ${SHA.toUpperCase()}`,
      /40-character SHA/,
    ],
    [
      "--match-head-commit last, without its SHA",
      `gh pr merge ${PR} --squash --match-head-commit`,
      /40-character SHA/,
    ],
    ["no PR number", `gh pr merge --squash --match-head-commit ${SHA}`, /by its number/],
    [
      "a branch for the PR",
      `gh pr merge p13/x --squash --match-head-commit ${SHA}`,
      /by its number/,
    ],
    ["a second number", `gh pr merge ${PR} 102 --squash --match-head-commit ${SHA}`, /"102"/],
    ["--subject", `${MERGE} --subject done`, /"--subject"/],
    ["-R", `${MERGE} -R owner/repo`, /"-R"/],
    ["a repeated flag", `${MERGE} --squash`, /--squash is given twice/],
    ["a second command", `${MERGE}; echo done`, plainly],
    ["&&", `git fetch && ${MERGE}`, plainly],
    ["a pipe", `${MERGE} | tail -1`, plainly],
    ["a redirect", `${MERGE} > out.txt`, plainly],
    ["a substitution", `gh pr merge $(echo ${PR}) --squash --match-head-commit ${SHA}`, plainly],
    ["a line break", `${MERGE}\necho done`, plainly],
    ["quotes", `gh pr merge "${PR}" --squash --match-head-commit ${SHA}`, plainly],
    ["bash -c", `bash -c "${MERGE}"`, plainly],
    ["echo", "echo gh pr merge", /doesn't start with "gh pr merge"/],
    ["gh.exe", `gh.exe pr merge ${PR} --squash --match-head-commit ${SHA}`, /doesn't start/],
    ["upper case", `GH PR MERGE ${PR} --squash --match-head-commit ${SHA}`, /doesn't start/],
  ]) {
    it(`refuses ${name} before any call`, () => {
      const { result, calls } = decideIn(() => {}, command);

      refused(result, pattern);
      assert.deepEqual(calls, []);
    });
  }

  it("takes the flags in any order, --delete-branch optional", () => {
    for (const command of [
      `gh pr merge --squash ${PR} --match-head-commit ${SHA}`,
      `gh pr merge --match-head-commit ${SHA} --delete-branch --squash ${PR}`,
    ]) {
      assert.deepEqual(decideIn(() => {}, command).result, { allow: true });
    }
  });
});

describe("the repository and the branch (D3, D4)", () => {
  it("a merge run in another repository is refused", () => {
    const { result } = decideIn((w) => w.replies.set(TOPLEVEL, ok("C:/elsewhere/other\n")));

    refused(result, /inside this repository/);
  });

  it("a merge run outside any repository is refused", () => {
    const { result } = decideIn((w) => w.replies.set(TOPLEVEL, exited(128)));

    refused(result, /inside this repository/);
  });

  it("a detached HEAD is refused", () => {
    const { result } = decideIn((w) => w.replies.set(SYMBOLIC, exited(1)));

    refused(result, /HEAD is detached/);
  });

  it("main as the branch is refused", () => {
    const { result } = decideIn((w) => w.replies.set(SYMBOLIC, ok("main\n")));

    refused(result, /can't be main/);
  });

  it("a SHA that isn't HEAD is refused", () => {
    const { result } = decideIn((w) => w.replies.set(HEAD, ok(`${OTHER}\n`)));

    refused(result, /0123456 isn't HEAD \(89abcde\)/);
  });

  it("a branch origin doesn't have is refused", () => {
    const { result } = decideIn((w) => w.replies.set(LS_REMOTE, ok("")));

    refused(result, /isn't on origin/);
  });

  it("a branch origin holds at another commit is refused", () => {
    const { result } = decideIn((w) =>
      w.replies.set(LS_REMOTE, ok(`${OTHER}\trefs/heads/${BRANCH}\n`)),
    );

    refused(result, /origin's p13\/p13\.1-example is at 89abcde, not 0123456/);
  });

  it("an origin that can't be reached is refused", () => {
    const { result } = decideIn((w) => w.replies.set(LS_REMOTE, exited(128)));

    refused(result, /"git ls-remote origin refs\/heads\/p13\/p13\.1-example" failed \(exit 128\)/);
  });
});

describe("the sensitive paths (D5, D6, D10)", () => {
  /** A file in each entry of the list: the file itself, or one inside the folder. */
  const samples = [
    ...SENSITIVE.files,
    ...SENSITIVE.folders.map((folder) => `${folder}example.rs`),
    ...SENSITIVE.names.map((name) => `crates/example/${name}`),
  ];
  for (const sample of samples) {
    it(`${sample} needs a human`, () => {
      const { result } = decideIn((w) => w.replies.set(DIFF, ok(z("M", sample))));

      assert.deepEqual(result, { allow: false, reason: `NEEDS_HUMAN: ${sample}` });
    });
  }

  it("names every sensitive path, in the diff's order", () => {
    const { result } = decideIn((w) =>
      w.replies.set(
        DIFF,
        ok(z("M", "justfile", "M", "src/lib.rs", "A", ".github/workflows/x.yml")),
      ),
    );

    assert.deepEqual(result, {
      allow: false,
      reason: "NEEDS_HUMAN: justfile, .github/workflows/x.yml",
    });
  });

  it("a root clippy.toml and a spike's Cargo.lock need a human", () => {
    const { result } = decideIn((w) =>
      w.replies.set(DIFF, ok(z("M", "clippy.toml", "M", "spikes/azalea/Cargo.lock"))),
    );

    assert.deepEqual(result, {
      allow: false,
      reason: "NEEDS_HUMAN: clippy.toml, spikes/azalea/Cargo.lock",
    });
  });

  it("upper case doesn't hide a path", () => {
    const { result } = decideIn((w) => w.replies.set(DIFF, ok(z("A", "Justfile"))));

    assert.deepEqual(result, { allow: false, reason: "NEEDS_HUMAN: Justfile" });
  });

  it("a file moved out of .github/ shows as its deletion (renames off)", () => {
    const { result, calls } = decideIn((w) =>
      w.replies.set(DIFF, ok(z("D", ".github/workflows/ci.yml", "A", "old-ci.yml"))),
    );

    assert.deepEqual(result, { allow: false, reason: "NEEDS_HUMAN: .github/workflows/ci.yml" });
    assert.ok(calls.includes(DIFF));
  });

  it("a modified or deleted ADR needs a human; a new one doesn't", () => {
    for (const status of ["M", "D", "T"]) {
      const { result } = decideIn((w) =>
        w.replies.set(DIFF, ok(z(status, "docs/adr/0022-deployment-and-phase-12-refinements.md"))),
      );

      assert.deepEqual(result, {
        allow: false,
        reason: "NEEDS_HUMAN: docs/adr/0022-deployment-and-phase-12-refinements.md",
      });
    }
    const { result } = decideIn((w) => w.replies.set(DIFF, ok(z("A", "docs/adr/0024-new.md"))));
    assert.deepEqual(result, { allow: true });
  });

  it("a diff it can't read is refused", () => {
    const { result } = decideIn((w) => w.replies.set(DIFF, ok("M\0src/lib.rs\0A\0")));

    refused(result, /couldn't read git diff's list/);
  });

  describe("Cargo.toml", () => {
    const MANIFEST = "crates/example/Cargo.toml";
    const before =
      '[package]\nname = "example"\ndescription = "one"\n\n[dependencies]\nserde.workspace = true\n';

    /** Decides with the manifest changed from `old` to `now` (null: missing on that side). */
    function manifest(status, old, now) {
      return decideIn((w) => {
        w.replies.set(DIFF, ok(z(status, MANIFEST)));
        if (old !== null) {
          w.replies.set(`git show ${BASE}:${MANIFEST}`, ok(old));
        }
        if (now !== null) {
          w.replies.set(`git show ${SHA}:${MANIFEST}`, ok(now));
        }
      }).result;
    }

    it("a dependency added needs a human", () => {
      const now = `${before}regex = "1"\n`;

      assert.deepEqual(manifest("M", before, now), {
        allow: false,
        reason: `NEEDS_HUMAN: ${MANIFEST}`,
      });
    });

    it("a lints change needs a human", () => {
      const old = `${before}\n[lints]\nworkspace = true\n`;
      const now = `${before}\n[lints.clippy]\nunwrap_used = "allow"\n`;

      assert.deepEqual(manifest("M", old, now), {
        allow: false,
        reason: `NEEDS_HUMAN: ${MANIFEST}`,
      });
    });

    it("a change outside the dependency and lints sections doesn't", () => {
      const now = before.replace('"one"', '"two"');

      assert.deepEqual(manifest("M", before, now), { allow: true });
    });

    it("a new crate's manifest with dependencies needs a human", () => {
      assert.deepEqual(manifest("A", null, before), {
        allow: false,
        reason: `NEEDS_HUMAN: ${MANIFEST}`,
      });
    });

    it("a deleted manifest with dependencies needs a human", () => {
      assert.deepEqual(manifest("D", before, null), {
        allow: false,
        reason: `NEEDS_HUMAN: ${MANIFEST}`,
      });
    });

    it("a manifest git can't show needs a human", () => {
      const { result } = decideIn((w) => {
        w.replies.set(DIFF, ok(z("M", MANIFEST)));
        w.replies.set(`git show ${BASE}:${MANIFEST}`, exited(128));
        w.replies.set(`git show ${SHA}:${MANIFEST}`, ok(before));
      });

      assert.deepEqual(result, { allow: false, reason: `NEEDS_HUMAN: ${MANIFEST}` });
    });

    it("the merge base is asked only when a manifest changed", () => {
      const { calls } = decideIn(() => {});

      assert.equal(calls.includes(MERGE_BASE), false);
    });
  });
});

describe("dependencyLines (D6)", () => {
  for (const [name, text] of [
    ["[dependencies]", '[dependencies]\nfoo = "1"\n'],
    ["[dev-dependencies]", '[dev-dependencies]\nfoo = "1"\n'],
    ["[build-dependencies]", '[build-dependencies]\nfoo = "1"\n'],
    ["a target's dependencies", "[target.'cfg(windows)'.dependencies]\nfoo = \"1\"\n"],
    [
      "a target triple's dev-dependencies",
      '[target.x86_64-pc-windows-msvc.dev-dependencies]\nfoo = "1"\n',
    ],
    ["[workspace.dependencies]", '[workspace.dependencies]\nfoo = "1"\n'],
    ["a dependency's own table", '[dependencies.foo]\nversion = "1"\n'],
    ["[lints]", "[lints]\nworkspace = true\n"],
    ["[lints.clippy]", '[lints.clippy]\nunwrap_used = "deny"\n'],
    ["[workspace.lints.rust]", '[workspace.lints.rust]\nunsafe_code = "forbid"\n'],
    ["[patch.crates-io]", '[patch.crates-io]\nfoo = { path = "../foo" }\n'],
    ["[replace]", '[replace]\n"foo:1.0.0" = { git = "https://example.com/foo" }\n'],
    ["a dotted key at the root", 'dependencies.foo = "1"\n'],
    ["lints.workspace at the root", "lints.workspace = true\n"],
    ["an inline table under a target", "[target.'cfg(windows)']\ndependencies = { foo = \"1\" }\n"],
  ]) {
    it(`reads ${name} as a dependency or lints section`, () => {
      assert.notDeepEqual(dependencyLines(text), []);
    });
  }

  for (const [name, text] of [
    ["[package]", '[package]\nname = "x"\ndescription = "a [dependencies] note"\n'],
    ["[features]", '[features]\ndefault = ["dep:foo"]\n'],
    ["[workspace]", '[workspace]\nmembers = ["crates/*"]\n'],
    ["a multi-line string", '[package]\ndescription = """\n[dependencies]\nfoo = "1"\n"""\n'],
    ["[profile.release]", "[profile.release]\nlto = true\n"],
  ]) {
    it(`reads nothing in ${name}`, () => {
      assert.deepEqual(dependencyLines(text), []);
    });
  }

  it("keeps a multi-line array's lines under its key", () => {
    const old = '[dependencies]\nfoo = { version = "1", features = [\n  "a",\n] }\n';
    const now = old.replace('"a"', '"b"');

    assert.notDeepEqual(dependencyLines(old), dependencyLines(now));
  });

  it("throws on a header it can't read", () => {
    assert.throws(() => dependencyLines('[dependencies\nfoo = "1"\n'));
    assert.throws(() => dependencyLines("[target.'cfg(windows).dependencies]\n"));
  });
});

describe("the verdict", () => {
  it("a missing verdict is refused", () => {
    const { result } = decideIn((w) => w.files.delete(VERDICT));

    refused(result, /no readable verdict\.json in target\/review\/p13-p13\.1-example\//);
  });

  it("a verdict that isn't JSON is refused", () => {
    const { result } = decideIn((w) => w.files.set(VERDICT, "APPROVE"));

    refused(result, /no readable verdict\.json/);
  });

  for (const word of ["REVISE", "NEEDS_HUMAN", "approve"]) {
    it(`a verdict of ${word} is refused`, () => {
      const { result } = decideIn((w) => w.files.set(VERDICT, verdict({ verdict: word })));

      refused(
        result,
        new RegExp(`verdict\\.json says ${word} for 0123456; a merge needs APPROVE for 0123456`),
      );
    });
  }

  it("an approval of another head is refused", () => {
    const { result } = decideIn((w) => w.files.set(VERDICT, verdict({ head: OTHER })));

    refused(result, /says APPROVE for 89abcde; a merge needs APPROVE for 0123456/);
  });
});

describe("the pull request (D2)", () => {
  for (const [name, change, pattern] of [
    [
      "on another branch",
      { headRefName: "p13/other" },
      /PR #101 is p13\/other's, not p13\/p13\.1-example's/,
    ],
    ["at another head", { headRefOid: OTHER }, /PR #101's head is 89abcde, not 0123456/],
    ["into another base", { baseRefName: "develop" }, /PR #101 goes into develop, not main/],
    ["merged already", { state: "MERGED" }, /PR #101 is MERGED, not OPEN/],
    ["closed", { state: "CLOSED" }, /PR #101 is CLOSED, not OPEN/],
    [
      "from another repository",
      { isCrossRepository: true },
      /PR #101 comes from another repository/,
    ],
  ]) {
    it(`a PR ${name} is refused`, () => {
      const { result } = decideIn((w) => w.replies.set(VIEW, view(change)));

      refused(result, pattern);
    });
  }

  it("a PR gh can't show is refused", () => {
    const { result } = decideIn((w) => w.replies.set(VIEW, exited(1)));

    refused(result, /"gh pr view 101 --json/);
  });

  it("a PR answer that isn't JSON is refused", () => {
    const { result } = decideIn((w) => w.replies.set(VIEW, ok("not json")));

    refused(result, /couldn't read PR #101/);
  });
});

describe("the checks (D7)", () => {
  /** The checks' reply: `buckets` by name, exiting with `status`. */
  function checks(status, buckets) {
    return exited(
      status,
      JSON.stringify(Object.entries(buckets).map(([name, bucket]) => ({ name, bucket }))),
    );
  }

  it("no checks yet is refused", () => {
    const { result } = decideIn((w) => w.replies.set(CHECKS, checks(0, {})));

    refused(result, /PR #101 has no checks yet/);
  });

  for (const bucket of ["fail", "pending", "skipping", "cancel"]) {
    it(`a check that is ${bucket} is refused`, () => {
      const { result } = decideIn((w) =>
        w.replies.set(CHECKS, checks(bucket === "pending" ? 8 : 1, { fmt: "pass", e2e: bucket })),
      );

      refused(result, new RegExp(`not every check passed: e2e \\(${bucket}\\)`));
    });
  }

  it("gh exiting non-zero though every check passed is refused", () => {
    const { result } = decideIn((w) => w.replies.set(CHECKS, checks(1, { fmt: "pass" })));

    refused(result, /gh pr checks exited 1/);
  });

  it("checks it can't read are refused", () => {
    const { result } = decideIn((w) => w.replies.set(CHECKS, exited(1, "no checks reported")));

    refused(result, /couldn't read PR #101's checks/);
  });
});

describe("the allowed merge", () => {
  it("an approved, pushed, green PR with no sensitive path is allowed after exactly these calls", () => {
    const { result, calls } = decideIn(() => {});

    assert.deepEqual(result, { allow: true });
    assert.deepEqual(calls, [TOPLEVEL, SYMBOLIC, HEAD, DIFF, LS_REMOTE, VIEW, CHECKS]);
  });

  it("PowerShell's tool is guarded the same way (U3)", () => {
    const w = world();
    const powershell = (command) => ({ ...input(command), tool_name: "PowerShell" });

    assert.deepEqual(decide(powershell(MERGE), w.deps), { allow: true });
    refused(decide(powershell(`${MERGE} --admin`), w.deps), /--admin is never used/);
  });

  it("a repository path in another spelling is the same repository", () => {
    const { result } = decideIn((w) => w.replies.set(TOPLEVEL, ok("C:/path/to/afkfleet/\n")));

    assert.deepEqual(result, { allow: true });
  });
});

describe("failing closed (D8)", () => {
  it("a call that throws refuses the merge", () => {
    const { result } = decideIn((w) => w.replies.delete(HEAD));

    refused(result, /the guard failed \(unexpected call: git rev-parse HEAD\)/);
  });
});

describe("the command line", () => {
  /** Runs the real hook with `stdin`. */
  function hook(stdin) {
    return spawnSync(process.execPath, [HOOK], { input: stdin, encoding: "utf8", timeout: 30_000 });
  }

  it("a command that isn't a merge exits 0 with nothing on stderr", () => {
    const run = hook(JSON.stringify(input("cargo nextest run")));

    assert.equal(run.status, 0);
    assert.equal(run.stderr, "");
  });

  it("a merge with --admin exits 2 and says why on stderr", () => {
    const run = hook(JSON.stringify(input(`${MERGE} --admin`)));

    assert.equal(run.status, 2);
    assert.match(run.stderr, /--admin is never used/);
  });

  it("input it can't read exits 2", () => {
    const run = hook("not json");

    assert.equal(run.status, 2);
    assert.match(run.stderr, /couldn't read the hook's input/);
  });
});

describe("the sensitive list (D10, D14)", () => {
  it("is exactly ADR-0023's list", () => {
    assert.deepEqual(SENSITIVE, {
      files: [
        "CLAUDE.md",
        "deny.toml",
        "biome.json",
        "rust-toolchain.toml",
        "justfile",
        "pnpm-lock.yaml",
        "pnpm-workspace.yaml",
        "docs/threat-model.md",
        "scripts/tree-fingerprint.mjs",
        "scripts/threat-model-citations.mjs",
        "scripts/merge-guard.test.mjs",
        "scripts/coverage-gates.mjs",
        "scripts/tauri-hardening.mjs",
        "apps/desktop/src-tauri/tauri.conf.json",
        "apps/desktop/src-tauri/tauri.check.conf.json",
        "apps/desktop/src-tauri/build.rs",
        "crates/fleet-server/tests/routes.rs",
        "crates/fleet-server/src/config/rate_limits.rs",
        "crates/fleet-server/src/config/auth.rs",
        "crates/fleet-server/src/http/handlers/auth.rs",
        "crates/fleet-server/src/http/handlers/mfa.rs",
        "crates/fleet-server/src/app/auth.rs",
        "crates/fleet-server/src/app/users.rs",
        "crates/fleet-server/src/infra/crypto/passwords.rs",
        "crates/fleet-server/src/infra/crypto/tokens.rs",
        "crates/fleet-server/src/ports/tokens.rs",
        "crates/fleet-server/src/http/routes.rs",
        "crates/fleet-server/src/app/lockout.rs",
        "crates/fleet-server/src/infra/crypto/attempt_keys.rs",
        "crates/fleet-server/src/ports/login_attempts.rs",
        "crates/fleet-server/src/app/challenges.rs",
        "crates/fleet-server/src/app/event_tickets.rs",
        "crates/fleet-server/src/http/ws.rs",
        "crates/fleet-server/src/http/handlers/events.rs",
        "crates/fleet-server/src/app/events.rs",
        "crates/fleet-server/src/app/mfa.rs",
        "crates/fleet-server/src/app/login.rs",
        "crates/fleet-server/src/app/invites.rs",
        "crates/fleet-server/src/infra/crypto/totp.rs",
        "crates/fleet-server/src/infra/crypto/recovery_codes.rs",
        "crates/fleet-server/src/ports/totp.rs",
        "crates/fleet-server/src/ports/recovery_codes.rs",
        "crates/fleet-api-types/src/secret.rs",
        "crates/fleet-server/src/infra/crypto/vault.rs",
        "crates/fleet-server/src/ports/vault.rs",
        "crates/fleet-startup/src/secret_file.rs",
        "crates/fleet-server/src/app/vault.rs",
        "crates/fleet-server/src/ports/envelopes.rs",
        "crates/fleet-server/src/infra/sqlite/envelopes.rs",
        "crates/fleet-server/src/ports/msauth.rs",
        "crates/fleet-server/src/app/enrollment.rs",
      ],
      folders: [
        ".github/",
        ".claude/",
        ".cargo/",
        ".config/",
        "deploy/",
        "patches/",
        "apps/desktop/src-tauri/capabilities/",
        "apps/desktop/src-tauri/permissions/",
        "apps/desktop/e2e/baselines/",
        "crates/fleet-core/src/auth/",
        "crates/fleet-server/src/http/extractors/",
        "crates/fleet-server/src/http/rate_limit/",
        "crates/fleet-server/src/infra/msauth/",
        "crates/fleet-server/src/app/session_tokens/",
        "crates/fleet-server/src/app/link_flows/",
        "crates/fleet-server/src/infra/pki/",
        "crates/fleet-server/src/app/agents/",
        "crates/fleet-server/src/grpc/",
        "crates/fleet-agent/src/managed/",
        "crates/fleet-agent/src/enroll/",
      ],
      names: ["clippy.toml", "Cargo.lock"],
    });
  });

  it("covers every entry of the security coverage gates", () => {
    const security = ["auth", "vault", "msauth", "control-plane", "agent-control-plane"];
    const entries = GATES.filter((gate) => security.includes(gate.name)).flatMap(
      (gate) => gate.prefixes,
    );
    assert.equal(entries.length > 40, true);

    const missing = entries.filter((entry) =>
      entry.endsWith("/") ? !SENSITIVE.folders.includes(entry) : !SENSITIVE.files.includes(entry),
    );

    assert.deepEqual(missing, []);
  });
});

describe("the settings (D14)", () => {
  /** .claude/settings.json, read fresh. */
  function settings() {
    return JSON.parse(readFileSync(SETTINGS, "utf8"));
  }

  /** Each rule for the Bash tool, then its PowerShell twin (U3). */
  function twins(rules) {
    return rules.flatMap((rule) => [`Bash(${rule})`, `PowerShell(${rule})`]);
  }

  it("hold the hook and the permissions alone", () => {
    assert.deepEqual(Object.keys(settings()).sort(), ["hooks", "permissions"]);
    assert.deepEqual(Object.keys(settings().permissions).sort(), ["allow", "ask", "deny"]);
  });

  it("run the guard before every Bash and PowerShell call, failing closed (D8)", () => {
    const project = "$" + "{CLAUDE_PROJECT_DIR}";

    assert.deepEqual(settings().hooks, {
      PreToolUse: [
        {
          matcher: "Bash|PowerShell",
          hooks: [
            {
              type: "command",
              command: "node",
              args: [`${project}/.claude/hooks/merge-guard.mjs`],
              onFailure: "block",
            },
          ],
        },
      ],
    });
  });

  it("allow exactly the pipeline's commands (ADR-0023, U2)", () => {
    assert.deepEqual(
      settings().permissions.allow,
      twins([
        "git push -u origin p*",
        "gh pr create *",
        "gh pr view *",
        "gh pr checks *",
        "gh pr diff *",
        "gh pr edit *",
        "gh run list *",
        "gh run view *",
        "gh pr merge * --squash --match-head-commit *",
      ]),
    );
  });

  it("ask for every other GitHub write, gh pr edit's outward flags included (U2, D12)", () => {
    assert.deepEqual(
      settings().permissions.ask,
      twins([
        "gh pr edit *--add-label*",
        "gh pr edit *--add-reviewer*",
        "gh pr edit *--add-assignee*",
        "gh pr edit *--add-project*",
        "gh pr edit *--milestone*",
        "gh pr edit *--base*",
        "gh pr comment *",
        "gh pr review *",
        "gh pr close *",
        "gh pr reopen *",
        "gh pr ready *",
        "gh pr lock *",
        "gh pr unlock *",
        "gh pr update-branch *",
        "gh pr revert *",
        "gh issue *",
        "gh label *",
        "gh release *",
        "gh repo *",
        "gh workflow *",
        "gh secret *",
        "gh variable *",
        "gh auth *",
        "gh gist *",
        "gh alias *",
        "gh extension *",
        "gh cache *",
        "gh project *",
        "gh run rerun *",
        "gh run cancel *",
        "gh run delete *",
      ]),
    );
  });

  it("deny pushes to main, force pushes, --admin, --auto, gh api and edits to the hooks", () => {
    assert.deepEqual(settings().permissions.deny, [
      ...twins([
        "git push * main*",
        "git push *:main*",
        "git push *refs/heads/main*",
        "git push --force*",
        "git push *--force*",
        "git push -f*",
        "git push * -f*",
        "git push *+*",
        "gh pr merge * --admin*",
        "gh pr merge *--admin*",
        "gh pr merge * --auto*",
        "gh pr merge *--auto*",
        "gh api *",
      ]),
      "Edit(/.claude/hooks/**)",
    ]);
  });
});
