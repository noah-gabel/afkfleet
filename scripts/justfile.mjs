// Reads the repository's justfile for the scripts' tests (ADR-0020, R124): a recipe's body
// lines, its parameters with their defaults as written, and its dependencies. The Rust
// tests read the same recipes through fleet-testkit's `justfile` module, which also checks
// what each default holds. Tests: scripts/justfile.test.mjs.
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

/** The repository's justfile. */
export const JUSTFILE = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "justfile");

/** The justfile's text. */
export function readJustfile() {
  return readFileSync(JUSTFILE, "utf8");
}

/** The index of `recipe`'s header among `lines`: its whole name, then a space or its colon. */
function headerIndex(lines, recipe) {
  const at = lines.findIndex(
    (line) => line.startsWith(`${recipe} `) || line.startsWith(`${recipe}:`),
  );
  if (at === -1) {
    throw new Error(`the justfile has no recipe ${recipe}`);
  }
  return at;
}

/**
 * `recipe`'s header in `text`, split at its colon (the first outside quotes and
 * parentheses): the parameters' part and the dependencies' part.
 */
function headerParts(recipe, text) {
  const lines = text.split("\n");
  const header = lines[headerIndex(lines, recipe)].slice(recipe.length);
  let depth = 0;
  let quoted = false;
  for (let at = 0; at < header.length; at += 1) {
    const c = header[at];
    if (c === '"') {
      quoted = !quoted;
    } else if (!quoted && c === "(") {
      depth += 1;
    } else if (!quoted && c === ")") {
      depth -= 1;
    } else if (!quoted && depth === 0 && c === ":") {
      return { parameters: header.slice(0, at), dependencies: header.slice(at + 1) };
    }
  }
  throw new Error(`recipe ${recipe}'s header has no colon`);
}

/** The lines of `recipe`'s body in `text`, without their indentation. */
export function recipeBody(recipe, text = readJustfile()) {
  const lines = text.split("\n");
  const body = [];
  for (const line of lines.slice(headerIndex(lines, recipe) + 1)) {
    if (!line.startsWith("    ")) {
      break;
    }
    body.push(line.trim());
  }
  return body;
}

/** The parameters of `recipe`'s header in `text`, by name, each with its default or null. */
export function recipeParameters(recipe, text = readJustfile()) {
  const { parameters: part } = headerParts(recipe, text);
  const parameters = {};
  for (const [, name, value] of part.matchAll(
    /(\$?[A-Za-z_][A-Za-z0-9_-]*)(?:=(\((?:[^()]|\([^()]*\))*\)|"[^"]*"))?/g,
  )) {
    parameters[name] = value ?? null;
  }
  return parameters;
}

/** The recipes `recipe` depends on in `text`, in order. */
export function recipeDependencies(recipe, text = readJustfile()) {
  return headerParts(recipe, text).dependencies.trim().split(/\s+/).filter(Boolean);
}
