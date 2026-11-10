// Tests that `.claude/settings.json` denies exactly what CLAUDE.md's hard limits say Claude Code
// can enforce (ADR-0025): every shell rule for the Bash and the PowerShell tool alike, every file
// rule for reading and editing, nothing allowed that a hard limit forbids, and no hook left from
// the merge guard (ADR-0023, superseded). Run with `just scripts-test`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

/** `.claude/settings.json`, read fresh. */
const settings = () =>
  JSON.parse(readFileSync(new URL("../.claude/settings.json", import.meta.url)));

/** The shell patterns each hard limit needs, denied for both shell tools. */
const SHELL = {
  secrets: ["*secrets/*", "*target/dod-run*", "*vault-key-*", "gh auth*"],
  merge: ["gh pr merge*"],
  "push to main": ["git push * main*", "git push *:main*", "git push *refs/heads/main*"],
  "force push": [
    "git push --force*",
    "git push *--force*",
    "git push -f*",
    "git push * -f*",
    "git push *+*",
  ],
  tags: ["git push *--tags*", "git push *--follow-tags*", "git push *refs/tags/*", "git push * v*"],
  "releases and settings": [
    "gh release*",
    "gh repo*",
    "gh secret*",
    "gh variable*",
    "gh ruleset*",
    "gh workflow enable*",
    "gh workflow disable*",
  ],
  "gh api writes": [
    "gh api * -X*",
    "gh api *--method*",
    "gh api * -f *",
    "gh api * -F *",
    "gh api *--field*",
    "gh api *--raw-field*",
    "gh api *--input*",
  ],
  "real systems": [
    "just test-real-account*",
    "just test-ms-account*",
    "just test-managed-account*",
    "just dod-*",
    "just updater-sign*",
    "*--profile manual*",
    "*-P manual*",
  ],
  "paid services": ["gh codespace*"],
  "irreversible cleanups": ["cargo clean*", "docker system prune*", "docker volume prune*"],
};

/** The secret files, denied for reading and editing. */
const FILES = [
  "/secrets/**",
  "**/.env*",
  "**/*.key",
  "**/*.pem",
  "**/vault-key-*",
  "/target/dod-run/**",
];

/** Claude Code's own settings, the user's: denied for editing (an Edit rule covers Write too). */
const SETTINGS = ["/.claude/settings*.json", "~/.claude/settings*.json"];

describe("the settings' hard limits (ADR-0025)", () => {
  for (const [limit, patterns] of Object.entries(SHELL)) {
    it(`deny ${limit} for Bash and PowerShell`, () => {
      const { deny } = settings().permissions;

      for (const pattern of patterns) {
        assert.ok(deny.includes(`Bash(${pattern})`), `Bash(${pattern})`);
        assert.ok(deny.includes(`PowerShell(${pattern})`), `PowerShell(${pattern})`);
      }
    });
  }

  it("deny reading and editing every secret file", () => {
    const { deny } = settings().permissions;

    for (const pattern of FILES) {
      assert.ok(deny.includes(`Read(${pattern})`), `Read(${pattern})`);
      assert.ok(deny.includes(`Edit(${pattern})`), `Edit(${pattern})`);
    }
  });

  it("deny editing Claude Code's own settings, the project's and the user's", () => {
    const { deny } = settings().permissions;

    for (const pattern of SETTINGS) {
      assert.ok(deny.includes(`Edit(${pattern})`), `Edit(${pattern})`);
    }
  });

  it("deny nothing else: every deny rule is one of the hard limits'", () => {
    const expected = new Set([
      ...Object.values(SHELL)
        .flat()
        .flatMap((pattern) => [`Bash(${pattern})`, `PowerShell(${pattern})`]),
      ...FILES.flatMap((pattern) => [`Read(${pattern})`, `Edit(${pattern})`]),
      ...SETTINGS.map((pattern) => `Edit(${pattern})`),
    ]);

    assert.deepEqual(
      settings().permissions.deny.filter((rule) => !expected.has(rule)),
      [],
    );
  });

  it("allow normal work broadly, and no rule names a merge, a release or a secret", () => {
    const { allow, ask } = settings().permissions;

    assert.ok(allow.includes("Bash") && allow.includes("Edit"), allow.join(", "));
    assert.equal(ask, undefined);
    for (const rule of allow) {
      assert.doesNotMatch(rule, /merge|release|secret|dod-run|vault-key|\.env|\.key/, rule);
    }
  });

  it("run no hook: the merge guard is gone (ADR-0023, superseded)", () => {
    assert.equal(settings().hooks, undefined);
  });
});
