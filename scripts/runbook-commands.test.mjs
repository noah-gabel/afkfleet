// Tests for scripts/runbook-commands.mjs: every command docs/runbook.md gives is run by a
// test, or its block says why not (Plan.md P12.6; ADR-0022, S45, S46). Run with
// `just scripts-test`, which also checks the real runbook here.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { check, checkRepository, commandLines, stackFilter, TESTED } from "./runbook-commands.mjs";

/** The Rust tests the fixtures cite, as `rustFiles` gives them (paths under crates/). */
const RUST = [
  {
    path: "fleet-integration/tests/integration/smoke/ops/mod.rs",
    source: [
      "#[tokio::test]",
      "async fn slow_ops_restore_after_a_crash() {}",
      "#[test]",
      "fn slow_other_thing() {}",
      "#[test]",
      "fn manual_real_account() {}",
    ].join("\n"),
  },
];

/** The other files the fixtures' rows name, by their path from the repository's root. */
const FILES = {
  "crates/fleet-integration/tests/integration/smoke/ops/steps.rs":
    'self.setup_sh_ok(&["check"], Root::No);\nself.ok(&["stop", "agent", "server"], SLOW);\n',
  "scripts/deploy-setup.test.mjs":
    'it("runs the folder lines", () => {\n  const lines = ["mkdir ~/afkfleet"];\n});\n',
};

const CONTEXT = {
  rust: RUST,
  read: (path) => FILES[path] ?? null,
  stack: /slow_(smoke|sftp|ops)_/,
};

const STEPS = "crates/fleet-integration/tests/integration/smoke/ops/steps.rs";

const CHECK = {
  line: "sh setup.sh check",
  test: "smoke::ops::slow_ops_restore_after_a_crash",
  file: STEPS,
  literal: '&["check"]',
};

const STOP = {
  line: "docker compose stop agent server",
  test: "smoke::ops::slow_ops_restore_after_a_crash",
  file: STEPS,
  literal: '&["stop", "agent", "server"]',
};

const MKDIR = {
  line: "mkdir ~/afkfleet",
  test: "scripts/deploy-setup.test.mjs › runs the folder lines",
  file: "scripts/deploy-setup.test.mjs",
  literal: '"mkdir ~/afkfleet"',
};

const MARKER = "> **Not run by afkfleet's tests:** they change the host's firewall.";

/** A runbook of `lines`. */
const doc = (...lines) => lines.join("\n");

describe("commandLines", () => {
  it("joins continuation lines and collapses spaces, skipping blank lines and comments", () => {
    const body = [
      { line: 4, text: "# make the folder" },
      { line: 5, text: "mkdir   ~/afkfleet" },
      { line: 6, text: "" },
      { line: 7, text: "sh setup.sh \\" },
      { line: 8, text: "    check" },
    ];

    assert.deepEqual(commandLines(body), [
      { line: 5, text: "mkdir ~/afkfleet" },
      { line: 7, text: "sh setup.sh check" },
    ]);
  });
});

describe("stackFilter", () => {
  it("reads test-stack's nextest filter", () => {
    const filter = stackFilter([
      'cargo nextest run -p fleet-integration --profile slow -E "test(/slow_(smoke|sftp|ops)_/)"',
    ]);

    assert.ok(filter.test("smoke::ops::slow_ops_restore_after_a_crash"));
    assert.ok(!filter.test("slow_mc_join"));
  });

  it("refuses a recipe without one", () => {
    assert.throws(() => stackFilter(["cargo nextest run --profile slow"]), /no test\(\/…\/\)/);
  });
});

describe("check", () => {
  it("passes a tabled line, a marked block and every kind of other block", () => {
    const markdown = doc(
      "## Install",
      "```sh",
      "mkdir ~/afkfleet",
      "sh setup.sh \\",
      "  check",
      "```",
      MARKER,
      "```sh",
      "sudo ufw allow 443",
      "```",
      MARKER,
      "```powershell",
      "Get-FileHash afkfleet-keys.tar",
      "```",
      "```ini",
      "KEY=value",
      "```",
      "```text",
      "ok: done",
      "```",
      "```toml",
      "[x]",
      "```",
      "```yaml",
      "a: b",
      "```",
      "Run `setup.sh`'s `check`, never by hand.",
    );

    assert.deepEqual(check(markdown, [CHECK, MKDIR], CONTEXT), []);
  });

  it("names a line no row holds, even beside tabled ones", () => {
    const markdown = doc("```sh", "sh setup.sh check", "sh setup.sh status", "```");

    assert.deepEqual(check(markdown, [CHECK], CONTEXT), [
      "line 3: `sh setup.sh status` is run by no test in the table, and its block isn't marked",
    ]);
  });

  it("checks a ~~~ block's commands as it checks a ``` block's", () => {
    const markdown = doc("~~~sh", "sh setup.sh check", "sh setup.sh status", "~~~");

    assert.deepEqual(check(markdown, [CHECK], CONTEXT), [
      "line 3: `sh setup.sh status` is run by no test in the table, and its block isn't marked",
    ]);
  });

  it("refuses a tabled line in a marked block", () => {
    const markdown = doc(MARKER, "```sh", "sh setup.sh check", "```");

    assert.deepEqual(check(markdown, [CHECK], CONTEXT), [
      "line 3: `sh setup.sh check` is in the table, but its block is marked as run by no test",
      "the table's `sh setup.sh check` isn't a command of the runbook's any more",
    ]);
  });

  it("refuses a malformed marker, and a marker with no command block after it", () => {
    const markdown = doc(
      "> **Not run by afkfleet's tests:** no full stop",
      "```sh",
      "sudo ufw allow 443",
      "```",
      MARKER,
      "",
      "Text.",
      MARKER,
      "```text",
      "ok",
      "```",
    );

    assert.deepEqual(check(markdown, [], CONTEXT), [
      "line 1: a marker is `> **Not run by afkfleet's tests:** <why, one sentence>.`",
      "line 3: `sudo ufw allow 443` is run by no test in the table, and its block isn't marked",
      "line 5: a marker with no command block after it",
      "line 8: a marker with no command block after it",
    ]);
  });

  it("refuses an unmarked powershell block: it runs on the operator's PC", () => {
    const markdown = doc("```powershell", "Get-FileHash afkfleet-keys.tar", "```");

    assert.deepEqual(check(markdown, [], CONTEXT), [
      "line 1: a powershell block runs on the operator's PC, which no test reaches: mark it",
    ]);
  });

  it("refuses a fence without a language, in another language, or never closed", () => {
    const markdown = doc("```", "ls", "```", "```bash", "ls", "```", "```sh", "ls");

    assert.deepEqual(check(markdown, [], CONTEXT), [
      "line 1: a fence without a language",
      "line 4: a fence in `bash`, not sh, powershell, text, ini, toml or yaml",
      "line 7: a fence that's never closed",
      "line 8: `ls` is run by no test in the table, and its block isn't marked",
    ]);
  });

  it("refuses a command in inline code, outside blocks", () => {
    const markdown = doc(
      "Run `docker compose ps`, `sudo sh setup.sh init`, `sh setup.sh check` or `curl -O x`.",
      "But `setup.sh`, `docker`, `shasum` and `sudoedit` are names.",
      "```sh",
      "sh setup.sh check",
      "```",
    );

    assert.deepEqual(check(markdown, [CHECK], CONTEXT), [
      "line 1: `docker compose ps` is a command in inline code: give it a block",
      "line 1: `sudo sh setup.sh init` is a command in inline code: give it a block",
      "line 1: `sh setup.sh check` is a command in inline code: give it a block",
      "line 1: `curl -O x` is a command in inline code: give it a block",
    ]);
  });

  it("refuses a row the runbook no longer holds, and a row twice", () => {
    const markdown = doc("```sh", "sh setup.sh check", "```");

    assert.deepEqual(check(markdown, [CHECK, CHECK, STOP], CONTEXT), [
      "the table holds `sh setup.sh check` twice",
      "the table's `docker compose stop agent server` isn't a command of the runbook's any more",
    ]);
  });

  it("refuses a row whose Rust test doesn't exist, isn't test-stack's, or is manual", () => {
    const rows = [
      { ...CHECK, test: "smoke::ops::slow_ops_gone" },
      { ...CHECK, line: "sh setup.sh a", test: "smoke::ops::slow_other_thing" },
      { ...CHECK, line: "sh setup.sh b", test: "smoke::ops::manual_real_account" },
      { ...CHECK, line: "sh setup.sh c", test: "nowhere::slow_ops_restore_after_a_crash" },
    ];
    const markdown = doc("```sh", ...rows.map((row) => row.line), "```");

    assert.deepEqual(check(markdown, rows, CONTEXT), [
      "the table's `sh setup.sh check`: `smoke::ops::slow_ops_gone` names no test under crates/ or apps/desktop/src-tauri/",
      "the table's `sh setup.sh a`: `smoke::ops::slow_other_thing` isn't one just test-stack runs",
      "the table's `sh setup.sh b`: `smoke::ops::manual_real_account` is a manual test, which only the user runs",
      "the table's `sh setup.sh c`: `nowhere::slow_ops_restore_after_a_crash` names no test under crates/ or apps/desktop/src-tauri/",
    ]);
  });

  it("refuses a row whose Node case, file or literal isn't there, or whose test is neither kind", () => {
    const rows = [
      { ...MKDIR, test: "scripts/deploy-setup.test.mjs › makes no folder" },
      { ...MKDIR, line: "cd ~/afkfleet", test: "scripts/gone.test.mjs › runs the folder lines" },
      { ...MKDIR, line: "ls", literal: '"ls"' },
      { ...MKDIR, line: "pwd", file: "scripts/gone.mjs" },
      { ...MKDIR, line: "true", test: "deploy-setup › runs the folder lines" },
    ];
    const markdown = doc("```sh", ...rows.map((row) => row.line), "```");

    assert.deepEqual(check(markdown, rows, CONTEXT), [
      'the table\'s `mkdir ~/afkfleet`: scripts/deploy-setup.test.mjs has no it("makes no folder"',
      "the table's `cd ~/afkfleet`: scripts/gone.test.mjs doesn't exist",
      "the table's `ls`: scripts/deploy-setup.test.mjs doesn't hold \"ls\"",
      "the table's `pwd`: scripts/gone.mjs doesn't exist",
      "the table's `true`: `deploy-setup › runs the folder lines` is neither a Rust path nor scripts/<name>.test.mjs › <case>",
    ]);
  });

  it("refuses a runbook without a command, which would prove nothing", () => {
    assert.deepEqual(check(doc("# Empty", "", "Nothing to run."), [], CONTEXT), [
      "the runbook gives no command, so the check proves nothing",
    ]);
  });
});

describe("docs/runbook.md", () => {
  it("has every command run by a test in the table, or its block marked", () => {
    assert.deepEqual(checkRepository(), []);
  });

  it("ties most of its commands to tests: the table isn't empty", () => {
    assert.ok(TESTED.length > 0);
  });
});
