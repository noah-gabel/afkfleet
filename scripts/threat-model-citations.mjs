// Checks that every test the threat model cites exists.
//
//   node scripts/threat-model-citations.mjs
//
// docs/threat-model.md's tables name the tests that verify each mitigation in their
// "Verified by" column, so a renamed or deleted test would leave it citing one that's gone
// (Plan.md P7.12, ADR-0016). Only that column counts, and only spans in backticks that are
// Rust paths: snake_case segments joined by `::` (`map::chat_text_and_sender_are_sanitized`)
// or a bare name. Anything else in backticks (`tests/connector.rs`, `cargo deny`,
// `unsafe_code = "forbid"`) is skipped.
//
// A citation's last segment must be a test function in a `.rs` file under crates/: a `fn`
// (`async` and `pub(crate)` allowed) after a test attribute (`#[test]`, `#[tokio::test…]`,
// `#[sqlx::test…]`, `#[rstest]`, a `proptest!` block's `#[test]`), with other attributes,
// doc comments and blank lines allowed in between. Each segment before it must be that
// file's stem, a folder on its path, or a `mod` declared inside it. Exits with 1, naming
// each citation it can't find.
//
// `just scripts-test` runs the check on the real files through the tests, so CI runs it.
// Tests: scripts/threat-model-citations.test.mjs.
import { readdirSync, readFileSync } from "node:fs";
import { join, relative, sep } from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

/** The column whose citations are checked. */
export const COLUMN = "Verified by";

/** A Rust path: snake_case segments joined by `::`. */
const RUST_PATH = /^[a-z_][a-z0-9_]*(?:::[a-z_][a-z0-9_]*)*$/;

/** The start of a test attribute. */
const TEST_ATTRIBUTE = /^#\[(?:test|tokio::test|sqlx::test|rstest)\b/;

/** A function's first line, its name captured. */
const FUNCTION = /^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([a-z_][a-z0-9_]*)\b/;

/** A module declaration, its name captured. */
const MODULE = /^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([a-z_][a-z0-9_]*)\s*[{;]/;

/**
 * Splits a table row into its cells: on every `|` that isn't escaped as `\|`, without
 * the row's outer pipes.
 */
export function cells(row) {
  const parts = row.trim().split(/(?<!\\)\|/);
  return parts.slice(1, parts.length - 1).map((cell) => cell.trim());
}

/**
 * Every "Verified by" cell of every table in `markdown`, as `{ line, text }`, `line`
 * counting from 1.
 */
export function verifiedByCells(markdown) {
  const found = [];
  let column = -1;
  markdown.split("\n").forEach((rawLine, index) => {
    const line = rawLine.endsWith("\r") ? rawLine.slice(0, -1) : rawLine;
    if (!line.trimStart().startsWith("|")) {
      column = -1;
      return;
    }
    const row = cells(line);
    if (column === -1) {
      column = row.indexOf(COLUMN);
      return;
    }
    if (row.every((cell) => /^:?-+:?$/.test(cell))) {
      return;
    }
    if (column < row.length) {
      found.push({ line: index + 1, text: row[column] });
    }
  });
  return found;
}

/** The citations in a cell: every span in backticks that is a Rust path. */
export function citations(text) {
  return [...text.matchAll(/`([^`]+)`/g)]
    .map((match) => match[1])
    .filter((span) => RUST_PATH.test(span));
}

/** A string literal, or a char literal, whose brackets don't count. */
const LITERAL = /"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)'/g;

/** How far `line` moves an attribute's bracket depth, outside its literals. */
function depth(line) {
  return [...line.replace(LITERAL, "")].reduce(
    (sum, char) => sum + (char === "[" ? 1 : char === "]" ? -1 : 0),
    0,
  );
}

/**
 * The names of the test functions in `source`: each `fn` after a test attribute, past
 * other attributes (also ones spanning lines), doc comments, comments and blank lines.
 */
export function testFunctions(source) {
  const names = [];
  const lines = source.split("\n").map((line) => line.trim());
  for (let index = 0; index < lines.length; index += 1) {
    if (!TEST_ATTRIBUTE.test(lines[index])) {
      continue;
    }
    let open = depth(lines[index]);
    let next = index + 1;
    while (next < lines.length) {
      const line = lines[next];
      if (open > 0) {
        open += depth(line);
      } else if (line.startsWith("#[")) {
        open = depth(line);
      } else if (line === "" || line.startsWith("//")) {
        // A doc comment, a comment or a blank line.
      } else {
        const match = FUNCTION.exec(line);
        if (match !== null) {
          names.push(match[1]);
        }
        break;
      }
      next += 1;
    }
  }
  return names;
}

/** The modules `source` declares, inline or as files. */
export function modules(source) {
  return source
    .split("\n")
    .map((line) => MODULE.exec(line))
    .filter((match) => match !== null)
    .map((match) => match[1]);
}

/**
 * The test functions of `files` (`{ path, source }`, `path` relative to the root with
 * `/`), by name: each with the names that may qualify it, the folders of its path, its
 * file's stem and the modules declared in it.
 */
export function indexTests(files) {
  const index = new Map();
  for (const { path, source } of files) {
    const parts = path.split("/");
    const stem = parts[parts.length - 1].replace(/\.rs$/, "");
    const qualifiers = new Set([...parts.slice(0, -1), stem, ...modules(source)]);
    for (const name of testFunctions(source)) {
      if (!index.has(name)) {
        index.set(name, []);
      }
      index.get(name).push({ path, qualifiers });
    }
  }
  return index;
}

/** Whether `citation` names a test in `index`. */
export function resolves(citation, index) {
  const segments = citation.split("::");
  const name = segments[segments.length - 1];
  const qualifiers = segments.slice(0, -1);
  return (index.get(name) ?? []).some((test) =>
    qualifiers.every((qualifier) => test.qualifiers.has(qualifier)),
  );
}

/**
 * Checks the threat model's citations against `files`. Returns one message per citation
 * that names no test, and one if the document cites none at all (the check would prove
 * nothing).
 */
export function check(markdown, files) {
  const index = indexTests(files);
  const problems = [];
  let cited = 0;
  for (const cell of verifiedByCells(markdown)) {
    for (const citation of citations(cell.text)) {
      cited += 1;
      if (!resolves(citation, index)) {
        problems.push(`line ${cell.line}: \`${citation}\` names no test under crates/`);
      }
    }
  }
  if (cited === 0) {
    problems.push("the threat model cites no test, so the check proves nothing");
  }
  return problems;
}

/** Every `.rs` file under `root`'s crates/, skipping build output, as `{ path, source }`. */
export function rustFiles(root) {
  const crates = join(root, "crates");
  const files = [];
  const walk = (folder) => {
    for (const entry of readdirSync(folder, { withFileTypes: true })) {
      const full = join(folder, entry.name);
      if (entry.isDirectory()) {
        if (entry.name !== "target" && !entry.name.startsWith(".")) {
          walk(full);
        }
      } else if (entry.isFile() && entry.name.endsWith(".rs")) {
        files.push({
          path: relative(crates, full).split(sep).join("/"),
          source: readFileSync(full, "utf8"),
        });
      }
    }
  };
  walk(crates);
  return files;
}

/** The repository's root: this script's folder's parent. */
export const ROOT = join(fileURLToPath(new URL(".", import.meta.url)), "..");

/** Checks the real threat model against the real crates. */
export function checkRepository(root = ROOT) {
  const markdown = readFileSync(join(root, "docs", "threat-model.md"), "utf8");
  return check(markdown, rustFiles(root));
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  const problems = checkRepository();
  for (const problem of problems) {
    console.error(`threat-model-citations: ${problem}`);
  }
  process.exit(problems.length === 0 ? 0 : 1);
}
