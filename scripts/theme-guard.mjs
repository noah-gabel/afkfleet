// Checks that every colour in the UI comes from a theme token (Plan.md P11.10, ADR-0020, S95).
//
//   node scripts/theme-guard.mjs
//
// A hard-coded colour looks right in one theme and wrong in the other, and nothing else would
// say so. The sources of packages/ui/src and apps/desktop/src (no tests, no test helpers, no
// generated types) may not hold, comments aside:
// - a Tailwind palette utility, `white` and `black` included, with any variant or opacity
// - a hex value or a colour function in a class's `[...]`; a named colour, `_` read as a space,
//   only in a colour utility's or an arbitrary property's
// - `rgb(`, `oklch(` and the like in any string, or a string that is entirely `#hex`
//   (`href` aside)
// - a literal after a colour key or attribute, shorthands included, or as a `--*` style value; or
//   in any string in such a key's expression (`fill={on ? "white" : "none"}`), unless that string
//   is only compared (`tone === "green"`)
// `currentColor`, `transparent`, `inherit`, `none`, `var(--x)` and `color-mix` over `var()` are
// fine, and prose never counts: each rule needs a colour context. There is no file allowlist.
// A fade like `[mask-image:linear-gradient(…black…)]` is refused too: use Tailwind v4's `mask-*`
// utilities.
// **Its limit:** a colour that reaches a key through a constant (`fill={SHADE}`) goes unseen, and
// Tailwind's palette reset can't catch it either, since it's no class (ADR-0020, R186).
//
// Every corner comes from the `--radius` scale, so one line can square them all (ADR-0020, U101):
// the theme defines only `rounded-sm`, `-md`, `-lg` and `-xl`, each a multiple of `--radius`, and
// this refuses any other step, an arbitrary `rounded-[…]`, a bare `rounded`, a fixed corner anywhere
// in a style key's expression, and `--radius` set in a class or a style, which would fix a whole
// subtree's corners. `rounded-full` (a real circle) and `rounded-none` stay fixed. **Its limits:** a
// text string holding "rounded" as a whole word is refused too, so such a text gets reworded, and a
// corner through a constant (`borderRadius: SIZE`) goes unseen, as a colour does (ADR-0020, R207).
//
// A native `<select>` lives only in components/NativeSelect.tsx, whose one look holds the tokens
// for the select and its options: WebView2 paints the open list from them, and a transparent
// select in dark mode opened a light list with light text, which no sweep can see, since the list
// isn't in the DOM (found at the user's hands-on of PR #87).
//
// styles.css may hold colour literals only as `--*` values directly in `:root`, `.dark` or the
// `prefers-color-scheme` block. Every token is in both `:root` and `.dark`, each `--color-*` maps
// to a token that exists, what the media block sets equals `.dark`'s, and Tailwind's own palette
// stays reset, so a class this scan misses gets no colour at all.
//
// `just scripts-test` runs the check on the real files through the tests, so CI runs it.
// Tests: scripts/theme-guard.test.mjs.
import { readdirSync, readFileSync } from "node:fs";
import { join, relative, sep } from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

/** Tailwind's colour scales. */
const SCALES =
  "red|orange|amber|yellow|lime|green|emerald|teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose|slate|gray|zinc|neutral|stone|taupe|mauve|mist|olive";

/** The utilities that take a colour. */
const UTILITIES =
  "bg|text|border(?:-[xytrblse])?|ring(?:-offset)?|inset-ring|outline|divide|shadow|inset-shadow|drop-shadow|text-shadow|accent|caret|fill|stroke|decoration|placeholder|from|via|to";

/** A palette utility, whatever comes before it, with an opacity if any. */
const PALETTE_CLASS = new RegExp(
  `(?<![\\w-])(?:${UTILITIES})-(?:(?:${SCALES})-\\d{2,3}|white|black)(?:/[\\w.%\\[\\]-]+)?(?![\\w-])`,
  "g",
);

/** A colour function's start. */
const FUNCTION = "(?<![\\w-])(?:rgba?|hsla?|hwb|lab|lch|oklab|oklch|color)\\(";

/** A hex value or a colour function. */
const LITERAL = `#[0-9a-fA-F]{3,8}(?![0-9a-zA-Z])|${FUNCTION}`;

/** A utility's arbitrary value holding a literal: `bg-[#fff]`. */
const ARBITRARY_VALUE = new RegExp(`-\\[[^\\]\\s]*(?:${LITERAL})[^\\]\\s]*\\]`, "g");

/** An arbitrary property holding a literal: `[color:#fff]`. */
const ARBITRARY_PROPERTY = new RegExp(`\\[[a-z-]+:[^\\]\\s]*(?:${LITERAL})[^\\]\\s]*\\]`, "g");

/** A colour utility's arbitrary value, in group 1: `bg-[red]`, `shadow-[0_0_0_1px_black]`. */
const UTILITY_VALUE = new RegExp(`(?<![\\w-])(?:${UTILITIES})-\\[([^\\]\\s]+)\\]`, "g");

/** The CSS properties that take a colour, shorthands included. */
const COLOUR_PROPERTIES =
  "color|background|background-color|background-image|border|border-color|border-top|border-right|border-bottom|border-left|border-top-color|border-right-color|border-bottom-color|border-left-color|outline|outline-color|fill|stroke|accent-color|caret-color|stop-color|flood-color|lighting-color|text-decoration|text-decoration-color|column-rule|column-rule-color|box-shadow|text-shadow";

/** An arbitrary property's value, in group 1: `[color:red]`, `[border:1px_solid_red]`. */
const PROPERTY_VALUE = /\[[a-z-]+:([^\]\s]+)\]/g;

/** The same properties as React's style keys and SVG's attributes spell them. */
const COLOUR_KEYS = COLOUR_PROPERTIES.replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());

/** A colour key or attribute with a string value, its value in group 2. */
const KEYED = new RegExp(
  `(?<![\\w-])["']?(?:${COLOUR_KEYS})["']?\\s*[=:]\\s*\\{?\\s*(["'\`])([^"'\`\\n]*)\\1`,
  "g",
);

/** Where a colour key's expression starts: an attribute's `={`, or a style key's `:`. */
const KEY_EXPRESSION = new RegExp(`(?<![\\w-])["']?(?:${COLOUR_KEYS})["']?\\s*(?:=\\s*\\{|:)`, "g");

/** A `--*` style value, in group 2. */
const STYLE_VARIABLE = /["']--[\w-]+["']\s*:\s*(["'`])([^"'`\n]*)\1/g;

/** CSS's named colours. `transparent` and `currentColor` aren't among them: they're fine. */
const NAMED = new Set(
  "aliceblue antiquewhite aqua aquamarine azure beige bisque black blanchedalmond blue blueviolet brown burlywood cadetblue chartreuse chocolate coral cornflowerblue cornsilk crimson cyan darkblue darkcyan darkgoldenrod darkgray darkgreen darkgrey darkkhaki darkmagenta darkolivegreen darkorange darkorchid darkred darksalmon darkseagreen darkslateblue darkslategray darkslategrey darkturquoise darkviolet deeppink deepskyblue dimgray dimgrey dodgerblue firebrick floralwhite forestgreen fuchsia gainsboro ghostwhite gold goldenrod gray green greenyellow grey honeydew hotpink indianred indigo ivory khaki lavender lavenderblush lawngreen lemonchiffon lightblue lightcoral lightcyan lightgoldenrodyellow lightgray lightgreen lightgrey lightpink lightsalmon lightseagreen lightskyblue lightslategray lightslategrey lightsteelblue lightyellow lime limegreen linen magenta maroon mediumaquamarine mediumblue mediumorchid mediumpurple mediumseagreen mediumslateblue mediumspringgreen mediumturquoise mediumvioletred midnightblue mintcream mistyrose moccasin navajowhite navy oldlace olive olivedrab orange orangered orchid palegoldenrod palegreen paleturquoise palevioletred papayawhip peachpuff peru pink plum powderblue purple rebeccapurple red rosybrown royalblue saddlebrown salmon sandybrown seagreen seashell sienna silver skyblue slateblue slategray slategrey snow springgreen steelblue tan teal thistle tomato turquoise violet wheat white whitesmoke yellow yellowgreen".split(
    " ",
  ),
);

/** Whether `value` names a colour among its words. */
function namesColour(value) {
  return value
    .toLowerCase()
    .split(/[\s,/():]+/)
    .some((word) => NAMED.has(word));
}

/** Whether `value` holds a hex value, a colour function, or names a colour. */
export function isColourLiteral(value) {
  return new RegExp(LITERAL).test(value) || namesColour(value);
}

/**
 * Where the expression starting at `from` in `code` ends: at the closer of what it opened, or for
 * a style key at a top-level `,` or `;`. Nothing inside `quoted`, which marks the strings, counts.
 */
function expressionEnd(code, quoted, from) {
  let depth = 0;
  for (let at = from; at < code.length; at += 1) {
    const char = code[at] ?? "";
    if (!quoted[at]) {
      if ("([{".includes(char)) {
        depth += 1;
      } else if (")]}".includes(char)) {
        if (depth === 0) {
          return at;
        }
        depth -= 1;
      } else if (depth === 0 && ",;".includes(char)) {
        return at;
      }
    }
  }
  return code.length;
}

/**
 * Whether the string of `length` at `start` in `code`, between its quotes, is an operand of a
 * comparison: `tone === "green"` only names a tone, and no colour reaches the key.
 */
function isCompared(code, start, length) {
  return (
    /[=!]==?\s*$/.test(code.slice(0, start - 1)) ||
    /^\s*[=!]==?/.test(code.slice(start + length + 1))
  );
}

/**
 * `source` read as TypeScript far enough to find what's text: `code` is `source` with comments
 * blanked, its offsets intact, and `strings` holds each string or template chunk as
 * `{ text, index }`. A quote that doesn't close on its line isn't a string: JSX text holds
 * apostrophes.
 */
export function lex(source) {
  const code = source.split("");
  const strings = [];
  const blank = (from, to) => {
    for (let at = from; at < to; at += 1) {
      if (code[at] !== "\n") {
        code[at] = " ";
      }
    }
  };
  // Reads code from `start`; inside a template's `${`, up to its closing brace.
  const readCode = (start, inTemplate) => {
    let at = start;
    let depth = 0;
    while (at < source.length) {
      const char = source[at];
      const next = source[at + 1];
      if (inTemplate && char === "}" && depth === 0) {
        return at + 1;
      }
      if (char === "{") {
        depth += 1;
      } else if (char === "}") {
        depth -= 1;
      }
      if (char === "/" && next === "/") {
        const end = source.indexOf("\n", at);
        const stop = end === -1 ? source.length : end;
        blank(at, stop);
        at = stop;
      } else if (char === "/" && next === "*") {
        const end = source.indexOf("*/", at + 2);
        const stop = end === -1 ? source.length : end + 2;
        blank(at, stop);
        at = stop;
      } else if (char === '"' || char === "'") {
        at = readQuoted(at, char);
      } else if (char === "`") {
        at = readTemplate(at + 1);
      } else {
        at += 1;
      }
    }
    return at;
  };
  const readQuoted = (open, quote) => {
    let at = open + 1;
    while (at < source.length && source[at] !== "\n") {
      if (source[at] === "\\") {
        at += 2;
      } else if (source[at] === quote) {
        strings.push({ text: source.slice(open + 1, at), index: open, start: open + 1 });
        return at + 1;
      } else {
        at += 1;
      }
    }
    return open + 1;
  };
  const readTemplate = (start) => {
    let at = start;
    let chunk = start;
    while (at < source.length) {
      if (source[at] === "\\") {
        at += 2;
      } else if (source[at] === "`") {
        strings.push({ text: source.slice(chunk, at), index: chunk, start: chunk });
        return at + 1;
      } else if (source[at] === "$" && source[at + 1] === "{") {
        strings.push({ text: source.slice(chunk, at), index: chunk, start: chunk });
        at = readCode(at + 2, true);
        chunk = at;
      } else {
        at += 1;
      }
    }
    return at;
  };
  readCode(0, false);
  return { code: code.join(""), strings };
}

/** The line, from 1, of offset `index` in `text`. */
function lineOf(text, index) {
  return text.slice(0, index).split("\n").length;
}

/** Every hard-coded colour in the source `source`, as `{ line, rule, match }`. */
export function scanSource(source) {
  const { code, strings } = lex(source);
  const found = [];
  const add = (rule, match, index) => found.push({ line: lineOf(source, index), rule, match });
  for (const match of code.matchAll(PALETTE_CLASS)) {
    add("palette utility", match[0], match.index);
  }
  for (const pattern of [ARBITRARY_VALUE, ARBITRARY_PROPERTY]) {
    for (const match of code.matchAll(pattern)) {
      add("arbitrary colour", match[0], match.index);
    }
  }
  const literal = new RegExp(LITERAL);
  for (const pattern of [UTILITY_VALUE, PROPERTY_VALUE]) {
    for (const match of code.matchAll(pattern)) {
      const value = match[1] ?? "";
      if (!literal.test(value) && namesColour(value.replaceAll("_", " "))) {
        add("arbitrary colour", match[0], match.index);
      }
    }
  }
  const functionInString = new RegExp(FUNCTION);
  for (const { text, index } of strings) {
    if (functionInString.test(text)) {
      add("colour function in a string", text, index);
    } else if (
      /^#[0-9a-fA-F]{3,8}$/.test(text) &&
      !/href\s*[=:]\s*\{?\s*$/.test(code.slice(0, index))
    ) {
      add("hex string", text, index);
    }
  }
  const flagged = new Set();
  for (const pattern of [KEYED, STYLE_VARIABLE]) {
    for (const match of code.matchAll(pattern)) {
      const value = match[2] ?? "";
      const start = match.index + match[0].length - value.length - 1;
      if (isColourLiteral(value) && !isCompared(code, start, value.length)) {
        add("literal colour value", match[0], match.index);
        flagged.add(match.index);
      }
    }
  }
  const quoted = new Array(code.length).fill(false);
  for (const { text, start } of strings) {
    quoted.fill(true, start, start + text.length);
  }
  for (const match of code.matchAll(KEY_EXPRESSION)) {
    if (!flagged.has(match.index)) {
      const from = match.index + match[0].length;
      const end = expressionEnd(code, quoted, from);
      for (const { text, start } of strings) {
        if (
          start >= from &&
          start < end &&
          isColourLiteral(text) &&
          !isCompared(code, start, text.length)
        ) {
          add("literal colour value", text, start);
        }
      }
    }
  }
  found.push(...cornersOffScale(source, code, strings));
  return found.sort((a, b) => a.line - b.line);
}

/** `css` with its comments blanked, its offsets intact. */
function withoutComments(css) {
  return css.replace(/\/\*[\s\S]*?\*\//g, (comment) => comment.replace(/[^\n]/g, " "));
}

/** The top-level rules of `css`, as `{ prelude, body, start, end }`, the body's offsets `css`'s. */
function rules(css, offset = 0) {
  const found = [];
  let at = 0;
  let preludeStart = 0;
  while (at < css.length) {
    const char = css[at];
    if (char === ";") {
      preludeStart = at + 1;
      at += 1;
    } else if (char === "{") {
      let depth = 1;
      let close = at + 1;
      while (close < css.length && depth > 0) {
        depth += css[close] === "{" ? 1 : css[close] === "}" ? -1 : 0;
        close += 1;
      }
      found.push({
        prelude: css.slice(preludeStart, at).trim(),
        body: css.slice(at + 1, close - 1),
        start: offset + at + 1,
        end: offset + close - 1,
      });
      preludeStart = close;
      at = close;
    } else {
      at += 1;
    }
  }
  return found;
}

/** The `--*` declarations directly in `body`, as a map, and each one's span. */
function tokens(body, offset) {
  const values = new Map();
  const spans = [];
  for (const match of body.matchAll(/(--[\w-]+)\s*:\s*([^;{}]+);/g)) {
    values.set(match[1], (match[2] ?? "").trim());
    spans.push([offset + match.index, offset + match.index + match[0].length]);
  }
  return { values, spans };
}

/** The corner steps the theme defines, each a multiple of `--radius` (ADR-0020, U101). */
const RADIUS_STEPS = ["--radius-sm", "--radius-md", "--radius-lg", "--radius-xl"];

/** Every problem with the stylesheet `source`, as `{ line, rule, match }`. */
export function checkStylesheet(source) {
  const css = withoutComments(source);
  const found = [];
  const add = (rule, match, index) => found.push({ line: lineOf(source, index), rule, match });
  const top = rules(css);
  const root = top.find((rule) => rule.prelude === ":root");
  const dark = top.find((rule) => rule.prelude === ".dark");
  const media = top.find((rule) =>
    /^@media\s*\(prefers-color-scheme:\s*dark\)$/.test(rule.prelude),
  );
  const reset = top.some(
    (rule) => rule.prelude === "@theme" && /--color-\*\s*:\s*initial\s*;/.test(rule.body),
  );
  const inline = top.find((rule) => rule.prelude === "@theme inline");
  if (root === undefined || dark === undefined || inline === undefined) {
    add("tokens", "no :root, .dark or @theme inline block", 0);
    return found;
  }
  if (!reset) {
    add("palette reset", "no @theme { --color-*: initial; } keeps Tailwind's palette out", 0);
  }
  const rootTokens = tokens(root.body, root.start);
  const darkTokens = tokens(dark.body, dark.start);
  const allowed = [...rootTokens.spans, ...darkTokens.spans];

  if (media !== undefined) {
    const inner = rules(media.body, media.start);
    for (const rule of inner) {
      const held = tokens(rule.body, rule.start);
      allowed.push(...held.spans);
      for (const [name, value] of held.values) {
        if (darkTokens.values.get(name) !== value) {
          add("media block", `${name}: ${value} isn't .dark's`, rule.start);
        }
      }
    }
  }

  // Colour literals anywhere but the token blocks: blank what's allowed, then read the rest.
  const rest = css.split("");
  for (const [from, to] of allowed) {
    for (let at = from; at < to; at += 1) {
      if (rest[at] !== "\n") {
        rest[at] = " ";
      }
    }
  }
  const remainder = rest.join("").replace(/@import[^;]*;/g, (line) => " ".repeat(line.length));
  for (const match of remainder.matchAll(new RegExp(LITERAL, "g"))) {
    add("colour outside the token blocks", match[0], match.index);
  }
  for (const match of remainder.matchAll(PALETTE_CLASS)) {
    add("palette utility", match[0], match.index);
  }
  const property = new RegExp(`(?<![\\w-])(?:${COLOUR_PROPERTIES})\\s*:\\s*([^;{}]+);`, "g");
  const literal = new RegExp(LITERAL);
  for (const match of remainder.matchAll(property)) {
    if (!literal.test(match[1] ?? "") && isColourLiteral(match[1] ?? "")) {
      add("colour outside the token blocks", match[0], match.index);
    }
  }

  const scale = "radius scale";
  if (
    !top.some(
      (rule) => rule.prelude === "@theme" && /--radius-\*\s*:\s*initial\s*;/.test(rule.body),
    )
  ) {
    add(scale, "no @theme { --radius-*: initial; } keeps Tailwind's radius steps out", 0);
  }
  // Every `@theme` block, as a step a later one redefines wins (PR #90's review).
  const themes = top.filter((rule) => rule.prelude.startsWith("@theme"));
  const steps = themes.flatMap((rule) =>
    [...tokens(rule.body, rule.start).values]
      .filter(([name]) => name.startsWith("--radius-"))
      .map(([name, value]) => [name, value, rule.start]),
  );
  for (const [name, value, start] of steps) {
    if (!RADIUS_STEPS.includes(name)) {
      add(scale, `${name} is off the scale`, start);
    } else if (!/^(?:var\(--radius\)|calc\(var\(--radius\) \* [\d.]+\))$/.test(value)) {
      add(scale, `${name}: ${value} isn't a multiple of --radius`, start);
    }
  }
  for (const name of RADIUS_STEPS) {
    if (!steps.some(([step]) => step === name)) {
      add(scale, `${name} is missing`, inline.start);
    }
  }
  for (const match of remainder.matchAll(/(?<![\w-])border(?:-[a-z]+)*-radius\s*:\s*([^;{}]+);/g)) {
    if (!(match[1] ?? "").startsWith("var(--radius")) {
      add(scale, match[0], match.index);
    }
  }
  for (const match of remainder.matchAll(/@apply\s+([^;{}]+);/g)) {
    for (const token of (match[1] ?? "").split(/\s+/)) {
      if (cornerOffScale(token)) {
        add(scale, `@apply ${token}`, match.index);
      }
    }
  }
  // Read from the whole sheet: blanking the token blocks would hide `--border-radius: 8px` in `:root`.
  const within = (rule, index) => index >= rule.start && index < rule.end;
  for (const match of css.matchAll(/(?<![\w-])(--[\w-]*radius[\w-]*)\s*:\s*([^;{}]+);/g)) {
    const defined =
      themes.some((rule) => within(rule, match.index)) ||
      (match[1] === "--radius" && within(root, match.index));
    if (!defined && !(match[2] ?? "").startsWith("var(--radius")) {
      add(scale, match[0], match.index);
    }
  }

  for (const name of darkTokens.values.keys()) {
    if (!rootTokens.values.has(name)) {
      add("tokens", `${name} is in .dark but not :root`, dark.start);
    }
  }
  for (const [name, value] of rootTokens.values) {
    if (isColourLiteral(value) && !darkTokens.values.has(name)) {
      add("tokens", `${name} is in :root but not .dark`, root.start);
    }
  }
  for (const [name, value] of tokens(inline.body, inline.start).values) {
    if (name.startsWith("--color-")) {
      const target = /^var\((--[\w-]+)\)$/.exec(value)?.[1];
      if (target === undefined || !rootTokens.values.has(target)) {
        add("tokens", `${name}: ${value} maps to no token`, inline.start);
      }
    }
  }
  const qr = "--qr-background";
  if (
    rootTokens.values.get(qr) === undefined ||
    rootTokens.values.get(qr) !== darkTokens.values.get(qr)
  ) {
    add("tokens", `${qr} must be the same in both themes: a QR code scans on its own light`, 0);
  }
  return found.sort((a, b) => a.line - b.line);
}

/** The one file a native select may be written in. */
export const NATIVE_SELECT = "packages/ui/src/components/NativeSelect.tsx";

/** What the shared look must hold: the tokens, for the select and for its options. */
const SELECT_LOOK = [
  "bg-background",
  "text-foreground",
  "[&_option]:bg-background",
  "[&_option]:text-foreground",
];

/** Where `source` writes a native select in code, as lines: never in a comment or a string. */
function selectsIn(source) {
  const { code, strings } = lex(source);
  const bare = code.split("");
  for (const { text, start } of strings) {
    for (let at = start; at < start + text.length; at += 1) {
      if (bare[at] !== "\n") {
        bare[at] = " ";
      }
    }
  }
  const lines = [];
  for (const match of bare.join("").matchAll(/<select(?![\w-])/g)) {
    lines.push(lineOf(source, match.index));
  }
  // A string names the tag here, so this one reads the code with its strings.
  for (const match of code.matchAll(/createElement\(\s*["'`]select["'`]/g)) {
    lines.push(lineOf(source, match.index));
  }
  return lines;
}

/**
 * Every problem with the native selects of `files`, each `{ path, source }`, as
 * `{ file, line, rule, match }`: a select outside NativeSelect.tsx, not exactly one inside it, or
 * a shared look that doesn't hold the tokens or makes the select see-through.
 */
export function checkSelects(files) {
  const rule = "native select";
  const problems = [];
  const shared = files.find((file) => file.path === NATIVE_SELECT);
  for (const { path, source } of files) {
    if (path !== NATIVE_SELECT) {
      for (const line of selectsIn(source)) {
        problems.push({ file: path, line, rule, match: "<select: use NativeSelect" });
      }
    }
  }
  if (shared === undefined) {
    problems.push({ file: NATIVE_SELECT, line: 1, rule, match: "the file is missing" });
    return problems;
  }
  const { code } = lex(shared.source);
  if (selectsIn(shared.source).length !== 1) {
    problems.push({
      file: NATIVE_SELECT,
      line: 1,
      rule,
      match: "it must render exactly one <select",
    });
  }
  const words = new Set(code.split(/[\s"'`]+/));
  for (const token of SELECT_LOOK) {
    if (!words.has(token)) {
      problems.push({ file: NATIVE_SELECT, line: 1, rule, match: `it must hold ${token}` });
    }
  }
  for (const match of code.matchAll(/(?<![\w-])bg-(?:transparent\b|[a-z-]+\/\d+)/g)) {
    problems.push({
      file: NATIVE_SELECT,
      line: lineOf(shared.source, match.index),
      rule,
      match: `${match[0]}: see-through, so the open list is painted light`,
    });
  }
  return problems;
}

/** A corner utility on the `--radius` scale, or a fixed one: a real circle, or no corner. */
const CORNER_ON_SCALE =
  /^rounded(?:-(?:[trblse]|tl|tr|bl|br|ss|se|es|ee))?-(?:sm|md|lg|xl|full|none)$/;

/** `token` without its variants or an important mark: what follows the last `:` outside `[...]`. */
function withoutVariants(token) {
  let depth = 0;
  let from = 0;
  for (let at = 0; at < token.length; at += 1) {
    const char = token[at];
    if (char === "[" || char === "(") {
      depth += 1;
    } else if (char === "]" || char === ")") {
      depth -= 1;
    } else if (char === ":" && depth === 0) {
      from = at + 1;
    }
  }
  return token.slice(from).replace(/^!|!$/g, "");
}

/** Whether the class `token` draws a corner off the `--radius` scale. */
function cornerOffScale(token) {
  const utility = withoutVariants(token);
  return (
    /^\[(?:border(?:-[a-z]+)*-radius|--[\w-]*radius[\w-]*):/.test(utility) ||
    (/^rounded(?:-|$)/.test(utility) && !CORNER_ON_SCALE.test(utility))
  );
}

/** A style key that sets a corner, its name in group 1; a class's `[--x:…]` isn't one. */
const CORNER_KEY =
  /(?<![\w[-])["']?(borderRadius|border(?:Top|Bottom|Start|End)(?:Left|Right|Start|End)Radius|--border-radius|--radius(?:-\w+)?)["']?\s*:/g;

/** Whether the expression in `code` between `from` and `end` holds a fixed corner. */
function holdsFixedCorner(code, strings, quoted, from, end) {
  const fixedString = strings.some(
    ({ text, start }) =>
      start >= from &&
      start < end &&
      text !== "" &&
      !text.startsWith("var(--radius") &&
      !isCompared(code, start, text.length),
  );
  if (fixedString) {
    return true;
  }
  for (const number of code.slice(from, end).matchAll(/(?<![\w.-])\d[\w.]*/g)) {
    const start = from + number.index;
    if (!quoted[start] && !isCompared(code, start, number[0].length)) {
      return true;
    }
  }
  return false;
}

/** Every corner in `source` off the `--radius` scale, as `{ line, rule, match }` (U101). */
function cornersOffScale(source, code, strings) {
  const found = [];
  const rule = "corner off the scale";
  for (const { text, index } of strings) {
    for (const token of text.split(/\s+/)) {
      if (cornerOffScale(token)) {
        found.push({ line: lineOf(source, index), rule, match: token });
      }
    }
  }
  const quoted = new Array(code.length).fill(false);
  for (const { text, start } of strings) {
    quoted.fill(true, start, start + text.length);
  }
  for (const match of code.matchAll(CORNER_KEY)) {
    const from = match.index + match[0].length;
    const end = expressionEnd(code, quoted, from);
    // `--radius` set here would fix a whole subtree's corners, whatever its value.
    if (
      (match[1] ?? "").startsWith("--radius") ||
      holdsFixedCorner(code, strings, quoted, from, end)
    ) {
      found.push({ line: lineOf(source, match.index), rule, match: code.slice(match.index, end) });
    }
  }
  return found;
}

/** Whether `path`, relative and with `/`, is a shipped source this guard reads. */
export function isScanned(path) {
  return (
    /\.tsx?$/.test(path) &&
    !/\.test\.tsx?$/.test(path) &&
    !path.includes("/src/test/") &&
    !path.includes("/src/generated/")
  );
}

/** The sources under `root`'s UI folders, as paths relative to `root` with `/`. */
export function sourceFiles(root) {
  const found = [];
  for (const folder of ["packages/ui/src", "apps/desktop/src"]) {
    for (const entry of readdirSync(join(root, folder), { recursive: true, withFileTypes: true })) {
      if (entry.isFile()) {
        const path = relative(root, join(entry.parentPath, entry.name)).split(sep).join("/");
        if (isScanned(path)) {
          found.push(path);
        }
      }
    }
  }
  return found.sort();
}

/** Every problem in the repository at `root`, as `{ file, line, rule, match }`. */
export function checkRepository(root) {
  const problems = [];
  const files = sourceFiles(root).map((path) => ({
    path,
    source: readFileSync(join(root, path), "utf8"),
  }));
  for (const { path, source } of files) {
    for (const finding of scanSource(source)) {
      problems.push({ file: path, ...finding });
    }
  }
  problems.push(...checkSelects(files));
  const styles = "packages/ui/src/styles.css";
  for (const finding of checkStylesheet(readFileSync(join(root, styles), "utf8"))) {
    problems.push({ file: styles, ...finding });
  }
  return problems;
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const problems = checkRepository(root);
  for (const { file, line, rule, match } of problems) {
    process.stderr.write(`${file}:${line}: ${rule}: ${match}\n`);
  }
  process.exit(problems.length === 0 ? 0 : 1);
}
