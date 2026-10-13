// Tests for threat-model-citations.mjs. Run with `just scripts-test`, which also checks the
// real threat model against the real crates here.
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { describe, it } from "node:test";

import {
  cells,
  check,
  checkRepository,
  citations,
  indexTests,
  modules,
  resolves,
  rustFiles,
  testFunctions,
  verifiedByCells,
} from "./threat-model-citations.mjs";

/** Rust source with every kind of test the crates hold, and functions that aren't tests. */
const SOURCE = `//! A module.

use super::*;

#[test]
fn a_plain_test() {}

/// A doc comment between the attribute's neighbours.
#[tokio::test(start_paused = true)]

/// Its doc comment.
async fn a_paused_test() {}

#[rstest]
#[case::short("a")]
#[case::spanning(
    "b",
    [1, 2],
)]
#[timeout(std::time::Duration::from_secs(5))]
fn a_test_with_cases(#[case] text: &str) {}

#[rstest]
#[case::escapes("\\u{1B}[31m", "red[31m", '[')]
fn brackets_in_its_strings_dont_count(#[case] text: &str, #[case] c: char) {}

#[sqlx::test(migrations = false)]
async fn a_database_test(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {}

#[test]
#[cfg_attr(windows, ignore = "Unix only")]
pub(crate) fn a_crate_visible_test() {}

proptest! {
    #[test]
    fn a_property_holds(value in 0_u32..10) {
        prop_assert!(value < 10);
    }
}

fn not_a_test() {}

#[derive(Debug)]
struct Helper;

mod tests {
    #[test]
    fn an_inner_test() {}
}
`;

const FILES = [
  { path: "fleet-core/src/chat/incoming.rs", source: SOURCE },
  { path: "fleet-mc/tests/online.rs", source: "#[tokio::test]\nasync fn slow_online() {}\n" },
];

const TABLE = `# A document

| Threat | Mitigation | Verified by |
|---|---|---|
| One | Escaped \\| pipes stay in their cell | \`incoming::a_plain_test\`, \`tests/connector.rs\` |
| Two | Something | P2.3 (\`chat::incoming::tests::an_inner_test\`), \`cargo deny\` |

Some text with \`incoming::nowhere\` outside any table.

| Other | Column |
|---|---|
| \`incoming::nowhere\` | x |
`;

describe("cells", () => {
  it("splits a row on its pipes, keeping escaped ones", () => {
    assert.deepEqual(cells("| a | b \\| c | d |"), ["a", "b \\| c", "d"]);
  });
});

describe("verifiedByCells", () => {
  it("reads only the Verified by column, past the separator row", () => {
    const found = verifiedByCells(TABLE);

    assert.deepEqual(
      found.map((cell) => cell.line),
      [5, 6],
    );
    assert.match(found[0].text, /^`incoming::a_plain_test`/);
  });

  it("reads Windows line endings", () => {
    assert.equal(verifiedByCells(TABLE.replaceAll("\n", "\r\n")).length, 2);
  });
});

describe("citations", () => {
  it("keeps Rust paths and bare names, and skips everything else", () => {
    const text =
      "`map::chat_is_clean`, `a_bare_test`, `tests/connector.rs`, `read.rs`, `fleet-mc`, " +
      '`cargo deny`, `unsafe_code = "forbid"`, `Upper::case`, `azalea_auth::certs`';

    assert.deepEqual(citations(text), ["map::chat_is_clean", "a_bare_test", "azalea_auth::certs"]);
  });
});

describe("testFunctions", () => {
  it("finds every kind of test and nothing else", () => {
    assert.deepEqual(testFunctions(SOURCE), [
      "a_plain_test",
      "a_paused_test",
      "a_test_with_cases",
      "brackets_in_its_strings_dont_count",
      "a_database_test",
      "a_crate_visible_test",
      "a_property_holds",
      "an_inner_test",
    ]);
  });

  it("stops at anything else after the attribute", () => {
    assert.deepEqual(testFunctions("#[test]\nlet x = 1;\nfn later() {}\n"), []);
  });
});

describe("modules", () => {
  it("lists inline and file modules", () => {
    assert.deepEqual(modules("mod tests {\n}\nmod support;\npub(crate) mod helpers;\n"), [
      "tests",
      "support",
      "helpers",
    ]);
  });
});

describe("resolves", () => {
  const index = indexTests(FILES);

  it("takes a bare name, a stem, a folder and a module as qualifiers", () => {
    for (const citation of [
      "a_plain_test",
      "incoming::a_plain_test",
      "chat::incoming::a_database_test",
      "incoming::tests::an_inner_test",
      "online::slow_online",
      "tests::slow_online",
    ]) {
      assert.ok(resolves(citation, index), citation);
    }
  });

  it("refuses a wrong qualifier, a function that isn't a test and an unknown name", () => {
    for (const citation of ["online::a_plain_test", "not_a_test", "incoming::missing"]) {
      assert.ok(!resolves(citation, index), citation);
    }
  });
});

describe("check", () => {
  it("names each citation that resolves to no test, with its line", () => {
    const problems = check(TABLE.replace("an_inner_test", "a_renamed_test"), FILES);

    assert.deepEqual(problems, [
      "line 6: `chat::incoming::tests::a_renamed_test` names no test under crates/ or apps/desktop/src-tauri/",
    ]);
  });

  it("passes when every citation resolves", () => {
    assert.deepEqual(check(TABLE, FILES), []);
  });

  it("fails a document that cites no test", () => {
    assert.deepEqual(check("| a | Verified by |\n|---|---|\n| x | P7.1 |\n", FILES), [
      "the threat model cites no test, so the check proves nothing",
    ]);
  });
});

describe("rustFiles", () => {
  /** A repository in a temporary folder with `files` (path: source), removed after `use`. */
  function withRepository(files, use) {
    const root = mkdtempSync(join(tmpdir(), "citations-"));
    try {
      for (const [path, source] of Object.entries(files)) {
        mkdirSync(dirname(join(root, path)), { recursive: true });
        writeFileSync(join(root, path), source);
      }
      use(root);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }

  it("reads the crates and the desktop crate, skipping build output, generated code and dependencies", () => {
    withRepository(
      {
        "crates/fleet-core/src/lib.rs": "// core",
        "crates/fleet-core/target/debug/build.rs": "// build output",
        "apps/desktop/src-tauri/src/keychain.rs": "// keychain",
        "apps/desktop/src-tauri/tests/acl.rs": "// acl",
        "apps/desktop/src-tauri/target/debug/out.rs": "// build output",
        "apps/desktop/src-tauri/gen/schemas/x.rs": "// generated",
        "apps/desktop/src-tauri/node_modules/pkg/y.rs": "// a dependency",
        "apps/desktop/src-tauri/.cache/z.rs": "// hidden",
        "apps/desktop/src/main.rs": "// outside src-tauri",
      },
      (root) => {
        const paths = rustFiles(root)
          .map((file) => file.path)
          .sort();

        assert.deepEqual(paths, [
          "apps/desktop/src-tauri/src/keychain.rs",
          "apps/desktop/src-tauri/tests/acl.rs",
          "fleet-core/src/lib.rs",
        ]);
      },
    );
  });

  it("resolves a citation of a desktop test by its file and folders", () => {
    withRepository(
      {
        "crates/fleet-core/src/lib.rs": "// core",
        "apps/desktop/src-tauri/tests/acl.rs": "#[test]\nfn a_core_command_is_refused() {}\n",
      },
      (root) => {
        const table = "| a | Verified by |\n|---|---|\n| x | `acl::a_core_command_is_refused` |\n";

        assert.deepEqual(check(table, rustFiles(root)), []);
      },
    );
  });
});

describe("the real threat model", () => {
  it("cites only tests that exist", () => {
    assert.deepEqual(checkRepository(), []);
  });
});
