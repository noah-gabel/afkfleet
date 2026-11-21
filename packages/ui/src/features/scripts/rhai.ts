// The editor's Rhai, a tokenizer of our own (Plan.md P15.24; ADR-0036, D16; ADR-0045). Pure: no
// view, no React.
//
// - **One tokenizer** for the highlighting (`rhaiLanguage`, CodeMirror's `StreamLanguage`) and
//   for the editor's own checks, completion and hover (`tokens`), so what's coloured as a game
//   object is what's checked as one.
// - **What it reads** is the language as fleet-script compiles it: line and nested block
//   comments, "…" strings (a backslash at a line's end carries one on), `…` strings across lines
//   with `${…}` inside, characters, numbers (`1_000`, `0x1F`, `1.5e3`), keywords, the API's
//   objects from the generated description, and a member after a dot. A raw string's `#"` is
//   marked invalid: the compiler refuses one (ADR-0040's amendment, A6).
// - **Indentation** follows the brackets open at a line's start; a line that starts by closing
//   one sits one unit back. Folding is by a line that ends with `{` (`braceFold`).
import { foldService, indentUnit, StreamLanguage, StringStream } from "@codemirror/language";
import type { EditorState } from "@codemirror/state";
import { tags } from "@lezer/highlight";

import { GAME_OBJECTS } from "./api";

/** What a token is, as the highlighting and the checks name it. */
export type TokenType =
  | "keyword"
  | "atom"
  | "number"
  | "string"
  | "comment"
  | "object"
  | "member"
  | "definition"
  | "dot"
  | "bracket"
  | "operator"
  | "name"
  | "invalid";

/** Rhai's keywords as fleet-script's sandbox has them. */
const KEYWORDS = new Set([
  "let",
  "const",
  "if",
  "else",
  "switch",
  "do",
  "while",
  "until",
  "loop",
  "for",
  "in",
  "break",
  "continue",
  "return",
  "throw",
  "try",
  "catch",
  "fn",
  "private",
  "this",
  "global",
  "is",
  "Fn",
]);

/** The words that are values. */
const ATOMS = new Set(["true", "false"]);

/** One open `…` string: in its text, or inside one of its `${…}` with the braces open at its start. */
interface Template {
  inText: boolean;
  braces: number;
}

/** Where the tokenizer is between lines. */
export interface RhaiState {
  /** Block comments open, nested. */
  comment: number;
  /** A "…" string a backslash carried past a line's end. */
  quote: boolean;
  /** The `…` strings open, innermost last. */
  templates: Template[];
  /** Braces open in code. */
  braces: number;
  /** Brackets of any kind open, for indentation. */
  depth: number;
  /** What the last token was, for a name after `fn` or after a dot. */
  after: "fn" | "dot" | "other";
}

/** A state at the text's start. */
export function startState(): RhaiState {
  return { comment: 0, quote: false, templates: [], braces: 0, depth: 0, after: "other" };
}

/** A copy of `state`, for a line's start. */
function copyState(state: RhaiState): RhaiState {
  return { ...state, templates: state.templates.map((template) => ({ ...template })) };
}

/** The rest of a block comment; `/*` inside one opens a nested one. */
function blockComment(stream: StringStream, state: RhaiState): TokenType {
  while (!stream.eol()) {
    if (stream.match("*/")) {
      state.comment -= 1;
      if (state.comment === 0) {
        return "comment";
      }
    } else if (stream.match("/*")) {
      state.comment += 1;
    } else {
      stream.next();
    }
  }
  return "comment";
}

/** The rest of a "…" string, which a backslash at the line's end carries on. */
function quoted(stream: StringStream, state: RhaiState): TokenType {
  state.quote = false;
  while (!stream.eol()) {
    const next = stream.next();
    if (next === "\\") {
      if (stream.eol()) {
        state.quote = true;
        return "string";
      }
      stream.next();
    } else if (next === '"') {
      return "string";
    }
  }
  return "string";
}

/** The text of the innermost `…` string, up to its end, a `${` or the line's end. */
function templateText(stream: StringStream, state: RhaiState, template: Template): TokenType {
  while (!stream.eol()) {
    if (stream.match("${", false)) {
      if (stream.current() === "") {
        stream.match("${");
        template.inText = false;
        template.braces = state.braces;
        state.depth += 1;
        return "bracket";
      }
      return "string";
    }
    const next = stream.next();
    if (next === "\\") {
      stream.next();
    } else if (next === "`") {
      state.templates.pop();
      return "string";
    }
  }
  return "string";
}

/** One token of code. */
function code(stream: StringStream, state: RhaiState): TokenType | null {
  if (stream.eatSpace()) {
    return null;
  }
  const after = state.after;
  state.after = "other";
  if (stream.match("//")) {
    stream.skipToEnd();
    return "comment";
  }
  if (stream.match("/*")) {
    state.comment = 1;
    return blockComment(stream, state);
  }
  if (stream.match('#"') || stream.match("##")) {
    return "invalid";
  }
  if (stream.match("#{")) {
    state.braces += 1;
    state.depth += 1;
    return "bracket";
  }
  const next = stream.peek() ?? "";
  if (next === '"') {
    stream.next();
    return quoted(stream, state);
  }
  if (next === "`") {
    stream.next();
    const template: Template = { inText: true, braces: 0 };
    state.templates.push(template);
    return templateText(stream, state, template);
  }
  if (next === "'") {
    stream.next();
    if (stream.next() === "\\") {
      stream.next();
    }
    stream.eat("'");
    return "string";
  }
  if (/\d/.test(next)) {
    if (
      stream.match(/^0x[\da-fA-F_]+/) ||
      stream.match(/^0o[0-7_]+/) ||
      stream.match(/^0b[01_]+/)
    ) {
      return "number";
    }
    stream.match(/^\d[\d_]*(\.\d[\d_]*)?([eE][+-]?\d+)?/);
    return "number";
  }
  if (/[A-Za-z_]/.test(next)) {
    stream.match(/^[A-Za-z_]\w*/);
    const word = stream.current();
    if (after === "dot") {
      return "member";
    }
    if (after === "fn") {
      return "definition";
    }
    if (KEYWORDS.has(word)) {
      state.after = word === "fn" ? "fn" : "other";
      return "keyword";
    }
    if (ATOMS.has(word)) {
      return "atom";
    }
    return GAME_OBJECTS.has(word) ? "object" : "name";
  }
  if (next === ".") {
    stream.next();
    if (stream.eat(".")) {
      stream.eat("=");
      return "operator";
    }
    state.after = "dot";
    return "dot";
  }
  if (next === "?" && stream.match("?.")) {
    state.after = "dot";
    return "dot";
  }
  stream.next();
  return bracketOrOperator(next, state);
}

/** A bracket, which opens or closes, or another operator. */
function bracketOrOperator(char: string, state: RhaiState): TokenType {
  switch (char) {
    case "{":
      state.braces += 1;
      state.depth += 1;
      return "bracket";
    case "}": {
      const template = state.templates.at(-1);
      if (template !== undefined && !template.inText && state.braces === template.braces) {
        template.inText = true;
      } else {
        state.braces = Math.max(0, state.braces - 1);
      }
      state.depth = Math.max(0, state.depth - 1);
      return "bracket";
    }
    case "(":
    case "[":
      state.depth += 1;
      return "bracket";
    case ")":
    case "]":
      state.depth = Math.max(0, state.depth - 1);
      return "bracket";
    default:
      return "operator";
  }
}

/** The next token from `stream`; see the module comment. */
export function token(stream: StringStream, state: RhaiState): TokenType | null {
  if (state.comment > 0) {
    return blockComment(stream, state);
  }
  if (state.quote) {
    return quoted(stream, state);
  }
  const template = state.templates.at(-1);
  if (template?.inText) {
    return templateText(stream, state, template);
  }
  return code(stream, state);
}

/** A line's indentation: the brackets open at its start, one fewer if it starts by closing one. */
function indent(state: RhaiState, textAfter: string, unit: number): number | null {
  if (state.comment > 0 || state.quote || state.templates.at(-1)?.inText === true) {
    return null;
  }
  const closing = /^\s*[}\])]/.test(textAfter) ? 1 : 0;
  return Math.max(0, state.depth - closing) * unit;
}

/** Rhai, for CodeMirror. */
export const rhaiLanguage = StreamLanguage.define<RhaiState>({
  name: "rhai",
  startState,
  copyState,
  token,
  indent: (state, textAfter, context) => indent(state, textAfter, context.unit),
  languageData: {
    commentTokens: { line: "//", block: { open: "/*", close: "*/" } },
    closeBrackets: { brackets: ["(", "[", "{", '"', "`"] },
    indentOnInput: /^\s*[}\])]$/,
  },
  tokenTable: {
    keyword: tags.keyword,
    atom: tags.bool,
    number: tags.number,
    string: tags.string,
    comment: tags.comment,
    object: tags.special(tags.variableName),
    member: tags.propertyName,
    definition: tags.function(tags.definition(tags.variableName)),
    dot: tags.derefOperator,
    bracket: tags.bracket,
    operator: tags.operator,
    name: tags.variableName,
    invalid: tags.invalid,
  },
});

/** A token of a text: what it is, and where, in UTF-16 offsets from the text's start. */
export interface Token {
  readonly type: TokenType;
  readonly from: number;
  readonly to: number;
  readonly text: string;
}

/** Every token of `text`, through the same tokenizer as the highlighting. */
export function tokens(text: string, unit = 4): Token[] {
  const found: Token[] = [];
  const state = startState();
  let offset = 0;
  for (const line of text.split("\n")) {
    const stream = new StringStream(line, 4, unit);
    while (!stream.eol()) {
      const type = token(stream, state);
      if (stream.pos === stream.start) {
        // A tokenizer that reads nothing would loop for ever: take one character.
        stream.next();
      }
      if (type !== null) {
        found.push({
          type,
          from: offset + stream.start,
          to: offset + stream.pos,
          text: stream.current(),
        });
      }
      stream.start = stream.pos;
    }
    offset += line.length + 1;
  }
  return found;
}

/** Folds a line that ends with `{`, outside a comment or a text, to its matching `}`. */
export const braceFold = foldService.of(
  (state: EditorState, lineStart: number, lineEnd: number) => {
    const line = state.doc.lineAt(lineStart);
    const opened = lastOpenBrace(line.text);
    if (opened === null) {
      return null;
    }
    const from = lineStart + opened + 1;
    const close = matchingBrace(state.doc.sliceString(from), indentUnitOf(state));
    if (close === null) {
      return null;
    }
    const to = from + close;
    return to > lineEnd ? { from, to } : null;
  },
);

/** The tokens' unit of indentation in `state`. */
function indentUnitOf(state: EditorState): number {
  return state.facet(indentUnit).length;
}

/** Where a line's last token is an opening brace, if it is one. */
function lastOpenBrace(line: string): number | null {
  const last = tokens(line)
    .filter((one) => one.type !== "comment")
    .at(-1);
  return last !== undefined && last.type === "bracket" && last.text.endsWith("{")
    ? last.to - 1
    : null;
}

/** The offset in `rest` of the `}` that closes a brace opened just before it. */
function matchingBrace(rest: string, unit: number): number | null {
  let depth = 1;
  for (const one of tokens(rest, unit)) {
    if (one.type !== "bracket") {
      continue;
    }
    if (one.text.endsWith("{")) {
      depth += 1;
    } else if (one.text === "}") {
      depth -= 1;
      if (depth === 0) {
        return one.from;
      }
    }
  }
  return null;
}
