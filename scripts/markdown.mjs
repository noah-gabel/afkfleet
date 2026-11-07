// Reads Markdown for the docs' checks (ADR-0022, S45, S49): GitHub's heading anchors,
// fenced blocks, links and inline code. A line inside a fence is code, never a heading, a
// link or inline code. A fence opens with three or more backticks or tildes and an
// optional language, at any indent (inside a list item too), and closes with at least as
// many of the same character alone on a line; an unclosed one runs to the end, as
// CommonMark has it. Headings are ATX headings (`#` to `######`
// and a space). Line numbers count from 1.
//
// Used by scripts/runbook-commands.mjs and the docs' tests. Tests: scripts/markdown.test.mjs.

/**
 * A fence's opening line: its indent, its backticks or tildes, and its info string (a
 * backtick fence's holds no backtick).
 */
const OPENING = /^(\s*)(?:(`{3,})([^`]*)|(~{3,})(.*))$/;

/** An ATX heading: its level's hashes and its text, closing hashes allowed. */
const HEADING = /^(#{1,6})\s+(.*?)(?:\s+#+)?\s*$/;

/** A link or an image: its text and its target. */
const LINK = /!?\[([^\]]*)\]\(([^)\s]+)\)/g;

/** An inline code span of single backticks. */
const CODE = /`([^`]+)`/g;

/** `markdown`'s lines, without a trailing `\r`. */
function lines(markdown) {
  return markdown.split("\n").map((line) => (line.endsWith("\r") ? line.slice(0, -1) : line));
}

/**
 * Walks `markdown`: every fenced block, and every line outside them as
 * `{ line, text }`.
 */
function walk(markdown) {
  const blocks = [];
  const prose = [];
  const all = lines(markdown);
  let lastText = null;
  let open = null;
  all.forEach((text, index) => {
    const line = index + 1;
    if (open !== null) {
      const close = /^\s*(`{3,}|~{3,})\s*$/.exec(text);
      if (close !== null && close[1][0] === open.char && close[1].length >= open.ticks) {
        open.block.closed = true;
        blocks.push(open.block);
        open = null;
        lastText = { line, text: text.trim() };
        return;
      }
      const indent = Math.min(open.indent, text.length - text.trimStart().length);
      open.block.body.push({ line, text: text.slice(indent) });
      return;
    }
    const opening = OPENING.exec(text);
    if (opening !== null) {
      const fence = opening[2] ?? opening[4];
      open = {
        char: fence[0],
        ticks: fence.length,
        indent: opening[1].length,
        block: {
          info: (opening[3] ?? opening[5]).trim().split(/\s+/)[0] ?? "",
          line,
          closed: false,
          body: [],
          before: lastText,
        },
      };
      return;
    }
    prose.push({ line, text });
    if (text.trim() !== "") {
      lastText = { line, text: text.trim() };
    }
  });
  if (open !== null) {
    blocks.push(open.block);
  }
  return { blocks, prose };
}

/**
 * GitHub's anchor for a heading's `text`: links read as their text, lower case, every
 * character but letters, digits, `_`, `-` and spaces dropped, then each space a hyphen.
 */
export function slug(text) {
  return text
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{M}\p{N}\p{Pc} -]/gu, "")
    .replace(/ /g, "-");
}

/**
 * The headings outside fences, as `{ level, text, slug, line }`; a repeated anchor gets
 * `-1`, `-2`, … as GitHub numbers it.
 */
export function headings(markdown) {
  const seen = new Map();
  const found = [];
  for (const { line, text } of walk(markdown).prose) {
    const match = HEADING.exec(text);
    if (match === null) {
      continue;
    }
    const base = slug(match[2]);
    const count = seen.get(base) ?? 0;
    seen.set(base, count + 1);
    found.push({
      level: match[1].length,
      text: match[2],
      slug: count === 0 ? base : `${base}-${count}`,
      line,
    });
  }
  return found;
}

/**
 * The fenced blocks, as `{ info, line, closed, body, before }`: `info` the language (`""`
 * without one), `line` the opening fence's, `body` its lines as `{ line, text }` without
 * the fence's indent, and `before` the last non-blank line above it, trimmed, or null.
 */
export function fences(markdown) {
  return walk(markdown).blocks;
}

/** The lines outside fences, as `{ line, text }`. */
export function prose(markdown) {
  return walk(markdown).prose;
}

/** `text` without its inline code spans. */
function withoutCode(text) {
  return text.replace(CODE, "");
}

/** The links (and images) outside fences and inline code, as `{ text, target, line }`. */
export function links(markdown) {
  return walk(markdown).prose.flatMap(({ line, text }) =>
    [...withoutCode(text).matchAll(LINK)].map((match) => ({
      text: match[1],
      target: match[2],
      line,
    })),
  );
}

/** The inline code spans outside fences, as `{ code, line }`. */
export function inlineCode(markdown) {
  return walk(markdown).prose.flatMap(({ line, text }) =>
    [...text.matchAll(CODE)].map((match) => ({ code: match[1], line })),
  );
}

/**
 * Each heading's section by its anchor, as `{ level, line, text }`: `text` the lines after
 * the heading until the next heading of its level or a higher one (fences included).
 */
export function sections(markdown) {
  const all = lines(markdown);
  const found = headings(markdown);
  const result = new Map();
  found.forEach((heading, index) => {
    const next = found.slice(index + 1).find((other) => other.level <= heading.level);
    const end = next === undefined ? all.length : next.line - 1;
    result.set(heading.slug, {
      level: heading.level,
      line: heading.line,
      text: all.slice(heading.line, end).join("\n"),
    });
  });
  return result;
}
