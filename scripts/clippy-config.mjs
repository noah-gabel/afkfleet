// Checks that every crate-local clippy.toml carries the root clippy.toml's settings.
//
//   node scripts/clippy-config.mjs
//
// Clippy reads the nearest clippy.toml and doesn't merge it with the root one, so a
// crate config that leaves out a root setting silently loses it: the test allowances,
// or the ban on unbounded channels (ADR-0011). A crate config may add settings and
// list entries of its own. Exits with 1 if any config misses a root setting.
// Tests: scripts/clippy-config.test.mjs, which also run this check on the repository.
import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

/** Folders that hold no workspace crate: build output, dependencies, throwaway spikes. */
const IGNORED_DIRS = new Set([".git", "node_modules", "target", "spikes"]);

/** The file names clippy reads its configuration from. */
const CONFIG_NAMES = new Set(["clippy.toml", ".clippy.toml"]);

/**
 * Parses the flat TOML subset that clippy.toml uses: `key = value` lines and lists.
 * Returns `{ scalars, lists }`: `scalars` maps a key to its raw value text, and `lists`
 * maps a key to the set of its entries. An entry is the `path` of an inline table
 * (`{ path = "…", … }`), or the string itself in a list of strings.
 * Throws on table headers and unclosed lists, which this check doesn't support.
 */
export function parseClippyConfig(text) {
  const scalars = new Map();
  const lists = new Map();
  const source = stripComments(text);
  let pos = 0;

  while (pos < source.length) {
    const lineEnd = endOfLine(source, pos);
    const line = source.slice(pos, lineEnd).trim();
    if (line === "") {
      pos = lineEnd + 1;
      continue;
    }
    if (line.startsWith("[")) {
      throw new Error(`unsupported clippy.toml syntax: table header \`${line}\``);
    }
    const match = /^([A-Za-z0-9_-]+)\s*=\s*/.exec(source.slice(pos, lineEnd).trimStart());
    if (match === null) {
      throw new Error(`unsupported clippy.toml syntax: \`${line}\``);
    }
    const valueStart = source.indexOf(match[0], pos) + match[0].length;
    if (source[valueStart] === "[") {
      const listEnd = closingBracket(source, valueStart);
      lists.set(match[1], listEntries(source.slice(valueStart + 1, listEnd)));
      pos = endOfLine(source, listEnd) + 1;
    } else {
      scalars.set(match[1], source.slice(valueStart, lineEnd).trim());
      pos = lineEnd + 1;
    }
  }
  return { scalars, lists };
}

/**
 * Compares a crate config against the root config. Returns one message per root
 * setting the crate config misses, changes, or whose list lacks a root entry.
 */
export function missingRootSettings(rootText, crateText) {
  const root = parseClippyConfig(rootText);
  const crate = parseClippyConfig(crateText);
  const problems = [];

  for (const [key, value] of root.scalars) {
    const crateValue = crate.scalars.get(key);
    if (crateValue === undefined) {
      problems.push(`\`${key} = ${value}\` is missing`);
    } else if (crateValue !== value) {
      problems.push(`\`${key}\` is \`${crateValue}\`, but the root clippy.toml sets \`${value}\``);
    }
  }
  for (const [key, entries] of root.lists) {
    const crateEntries = crate.lists.get(key) ?? new Set();
    for (const entry of entries) {
      if (!crateEntries.has(entry)) {
        problems.push(`\`${key}\` is missing \`${entry}\``);
      }
    }
  }
  return problems;
}

/**
 * Returns every clippy config below `repoRoot` except the root one, as sorted
 * repository-relative paths with `/` separators. Skips build output, dependencies and spikes.
 */
export function findCrateConfigs(repoRoot) {
  const found = [];
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        if (!IGNORED_DIRS.has(entry.name)) {
          walk(full);
        }
      } else if (CONFIG_NAMES.has(entry.name) && dir !== repoRoot) {
        found.push(path.relative(repoRoot, full).replaceAll("\\", "/"));
      }
    }
  };
  walk(repoRoot);
  return found.sort();
}

/**
 * Checks every crate-local clippy config in the repository against the root one.
 * Returns `{ file, problems }` for each config that misses a root setting.
 */
export function checkRepo(repoRoot) {
  const rootText = readFileSync(path.join(repoRoot, "clippy.toml"), "utf8");
  return findCrateConfigs(repoRoot)
    .map((file) => ({
      file,
      problems: missingRootSettings(rootText, readFileSync(path.join(repoRoot, file), "utf8")),
    }))
    .filter((result) => result.problems.length > 0);
}

/** Replaces every `#` comment outside a string with nothing, keeping the line breaks. */
function stripComments(text) {
  let out = "";
  let inString = false;
  for (let i = 0; i < text.length; i += 1) {
    const char = text[i];
    if (inString) {
      out += char;
      if (char === "\\") {
        out += text[i + 1] ?? "";
        i += 1;
      } else if (char === '"') {
        inString = false;
      }
    } else if (char === '"') {
      inString = true;
      out += char;
    } else if (char === "#") {
      i = endOfLine(text, i) - 1;
    } else {
      out += char;
    }
  }
  return out;
}

/** Returns the index of the `\n` that ends the line at `pos`, or the text's length. */
function endOfLine(text, pos) {
  const end = text.indexOf("\n", pos);
  return end === -1 ? text.length : end;
}

/** Returns the index of the `]` that closes the list opened at `open`, skipping strings. */
function closingBracket(text, open) {
  let depth = 0;
  let inString = false;
  for (let i = open; i < text.length; i += 1) {
    const char = text[i];
    if (inString) {
      if (char === "\\") {
        i += 1;
      } else if (char === '"') {
        inString = false;
      }
    } else if (char === '"') {
      inString = true;
    } else if (char === "[") {
      depth += 1;
    } else if (char === "]") {
      depth -= 1;
      if (depth === 0) {
        return i;
      }
    }
  }
  throw new Error("unsupported clippy.toml syntax: unclosed list");
}

/** Returns a list's entries: the `path` of each inline table, or each plain string. */
function listEntries(body) {
  const pattern = body.includes("{") ? /\bpath\s*=\s*"((?:[^"\\]|\\.)*)"/g : /"((?:[^"\\]|\\.)*)"/g;
  return new Set([...body.matchAll(pattern)].map((match) => match[1]));
}

function main() {
  const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  const results = checkRepo(repoRoot);
  for (const { file, problems } of results) {
    for (const problem of problems) {
      console.error(`${file}: ${problem}`);
    }
  }
  if (results.length > 0) {
    console.error("crate-local clippy.toml files must repeat every root setting (ADR-0011)");
    return 1;
  }
  console.log("every crate-local clippy.toml carries the root settings");
  return 0;
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main();
}
