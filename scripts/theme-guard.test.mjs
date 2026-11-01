// Tests for theme-guard.mjs. Run with `just scripts-test`, which also checks the real UI here.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  checkRepository,
  checkSelects,
  checkStylesheet,
  isColourLiteral,
  isScanned,
  lex,
  NATIVE_SELECT,
  scanSource,
  sourceFiles,
} from "./theme-guard.mjs";

/** What opens a template's expression, in a test's source text. */
const HOLE = "\u0024{";

/** The rules `source` breaks. */
function rulesOf(source) {
  return scanSource(source).map((finding) => finding.rule);
}

describe("a hard-coded colour in a source", () => {
  for (const [name, source] of [
    ["a white background", '<img className="size-48 bg-white p-2" />'],
    ["black text", '<p className="text-black" />'],
    ["a palette shade", 'const a = "text-red-500";'],
    ["a shade with a variant", 'const a = "dark:hover:bg-emerald-900";'],
    ["a shade with an opacity", 'const a = "bg-black/50";'],
    ["an important one", 'const a = "!border-slate-200";'],
    ["a group variant", 'const a = "[&>svg]:fill-blue-400";'],
    ["a ring's", 'const a = "focus-visible:ring-offset-white";'],
  ]) {
    it(`refuses ${name}`, () => {
      assert.deepEqual(rulesOf(source), ["palette utility"]);
    });
  }

  for (const [name, source] of [
    ["a hex class", 'const a = "bg-[#fff]";'],
    ["a function class", 'const a = "text-[rgb(0,0,0)]";'],
    ["an oklch class", 'const a = "border-[oklch(0.5_0_0)]";'],
    ["an arbitrary property's hex", 'const a = "[color:#123456]";'],
    ["a named colour in a utility", 'const a = "bg-[red]";'],
    ["a named colour with its type", 'const a = "text-[color:Tomato]";'],
    ["a named colour in a property", 'const a = "[background-color:navy]";'],
  ]) {
    it(`refuses ${name}`, () => {
      assert.ok(rulesOf(source).includes("arbitrary colour"), source);
    });
  }

  for (const [name, source] of [
    ["rgb in a string", 'const shade = "rgb(1, 2, 3)";'],
    ["oklch in a template", `const shade = \`oklch(0.5 0 ${HOLE}hue})\`;`],
    ["hsla in a string", "const shade = 'hsla(1, 2%, 3%, 0.5)';"],
  ]) {
    it(`refuses ${name}`, () => {
      assert.ok(rulesOf(source).includes("colour function in a string"), source);
    });
  }

  it("refuses a string that is a whole hex value", () => {
    assert.deepEqual(rulesOf('const shade = "#a1b2c3";'), ["hex string"]);
  });

  for (const [name, source] of [
    ["a style's colour", 'const s = <p style={{ color: "red" }} />;'],
    ["a style's background hex", 'const s = <p style={{ backgroundColor: "#fff" }} />;'],
    ["an SVG's fill", '<path fill="#000" />'],
    ["an SVG's stroke by name", '<path stroke="black" />'],
    ["a stop's colour", '<stop stopColor="rebeccapurple" />'],
    ["a variable's value", 'const s = { "--normal-bg": "white" };'],
    ["a variable's hex", 'const s = { "--normal-bg": "#123" };'],
  ]) {
    it(`refuses ${name}`, () => {
      assert.ok(rulesOf(source).includes("literal colour value"), source);
    });
  }

  it("says which line", () => {
    assert.equal(scanSource('\n\n<p className="bg-white" />')[0]?.line, 3);
  });
});

describe("what's fine in a source", () => {
  for (const [name, source] of [
    ["a token's class", 'const a = "bg-background text-muted-foreground border-border";'],
    ["a token with an opacity", 'const a = "hover:bg-destructive/20";'],
    ["transparent and the current colour", 'const a = "bg-transparent text-current";'],
    ["fill none and current", '<path fill="none" stroke="currentColor" />'],
    ["inherit", 'const s = { color: "inherit" };'],
    ["a variable's var", 'const s = { "--normal-bg": "var(--popover)" };'],
    ["color-mix over var", 'const a = "hover:bg-[color-mix(in_oklch,var(--a),var(--b)_5%)]";'],
    ["a data attribute's word", 'const a = "data-[color=red]:hidden";'],
    ["a word in a text", "<p>The red team wins, and black is a word.</p>"],
    ["a colour word in a string", 'const text = "Pick red or blue";'],
    ["a number after a hash in a text", 'const text = "Bot #123 is up";'],
    ["an href to an anchor", '<a href="#abc" />'],
    ["a key that only looks like one", 'const a = { background: "muted" };'],
    ["a private field in brackets", "const a = list[this.#fade];"],
    ["a function named color", "const a = color(1);"],
    ["an arrow's to", 'const a = "go-to-white";'],
    ["a line comment", '// bg-white and rgb(0, 0, 0) were here\nconst a = "b";'],
    ["a block comment", '/* text-red-500 */ const a = "b";'],
    ["a JSX comment", '<p>{/* fill="#000" */}</p>'],
  ]) {
    it(`takes ${name}`, () => {
      assert.deepEqual(scanSource(source), []);
    });
  }
});

describe("lex", () => {
  it("finds strings, template chunks and strings inside a template's expressions", () => {
    const { strings } = lex(`const a = "one"; const b = \`two ${HOLE}f("three")} four\`;`);
    assert.deepEqual(
      strings.map((found) => found.text),
      ["one", "two ", "three", " four"],
    );
  });

  it("treats an apostrophe in JSX text as no string", () => {
    const { strings } = lex('<p>Couldn\'t load</p>\nconst a = "one";');
    assert.deepEqual(
      strings.map((found) => found.text),
      ["one"],
    );
  });

  it("keeps a comment's offsets, and a // inside a string", () => {
    const source = '// gone\nconst a = "https://example.com";';
    const { code, strings } = lex(source);
    assert.equal(code.length, source.length);
    assert.ok(!code.includes("gone"));
    assert.deepEqual(
      strings.map((found) => found.text),
      ["https://example.com"],
    );
  });
});

describe("isColourLiteral", () => {
  for (const value of ["#fff", "rgb(0 0 0)", "Red", "1px solid black", "oklch(0.5 0 0)"]) {
    it(`says yes to ${value}`, () => assert.equal(isColourLiteral(value), true));
  }
  for (const value of ["var(--x)", "currentColor", "transparent", "inherit", "none", "muted"]) {
    it(`says no to ${value}`, () => assert.equal(isColourLiteral(value), false));
  }
});

describe("isScanned", () => {
  for (const path of [
    "packages/ui/src/App.tsx",
    "apps/desktop/src/main.tsx",
    "packages/ui/src/a.ts",
  ]) {
    it(`reads ${path}`, () => assert.equal(isScanned(path), true));
  }
  for (const path of [
    "packages/ui/src/App.test.tsx",
    "packages/ui/src/a.test.ts",
    "packages/ui/src/test/fixtures.ts",
    "packages/ui/src/generated/Bot.ts",
    "packages/ui/src/styles.css",
  ]) {
    it(`skips ${path}`, () => assert.equal(isScanned(path), false));
  }
});

/** A stylesheet that passes, with `changes` applied to its text. */
function stylesheet(changes = (css) => css) {
  return changes(`@import "tailwindcss" source(".");
@theme { --color-*: initial; }
@theme inline {
  --radius-sm: calc(var(--radius) - 4px);
  --color-background: var(--background);
  --color-qr-background: var(--qr-background);
}
:root {
  color-scheme: light;
  --radius: 0.625rem;
  --background: #ffffff;
  --qr-background: #fff;
}
.dark {
  color-scheme: dark;
  --background: #0a0a0a;
  --qr-background: #fff;
}
@media (prefers-color-scheme: dark) {
  :root:not(.light) {
    color-scheme: dark;
    --background: #0a0a0a;
  }
}
@layer base {
  body {
    @apply bg-background text-foreground;
  }
}
`);
}

/** The problems' rules for a stylesheet changed by `changes`. */
function cssRules(changes) {
  return checkStylesheet(stylesheet(changes)).map((problem) => problem.rule);
}

describe("the stylesheet", () => {
  it("takes the one above", () => {
    assert.deepEqual(checkStylesheet(stylesheet()), []);
  });

  it("refuses a colour in a rule, outside the token blocks", () => {
    assert.deepEqual(
      cssRules((css) => css.replace("@apply bg-background text-foreground;", "color: #fff;")),
      ["colour outside the token blocks"],
    );
  });

  it("refuses a named colour in a rule", () => {
    assert.deepEqual(
      cssRules((css) => css.replace("@apply bg-background text-foreground;", "color: red;")),
      ["colour outside the token blocks"],
    );
  });

  it("refuses a palette utility through @apply", () => {
    assert.deepEqual(
      cssRules((css) => css.replace("@apply bg-background", "@apply bg-white")),
      ["palette utility"],
    );
  });

  it("refuses a literal in @theme inline", () => {
    const rules = cssRules((css) =>
      css.replace("--color-background: var(--background);", "--color-background: #fff;"),
    );
    assert.ok(rules.includes("colour outside the token blocks"), String(rules));
    assert.ok(rules.includes("tokens"), String(rules));
  });

  it("refuses a token only .dark holds", () => {
    assert.deepEqual(
      cssRules((css) =>
        css.replace(
          "--background: #0a0a0a;\n  --qr",
          "--background: #0a0a0a;\n  --x: #000;\n  --qr",
        ),
      ),
      ["tokens"],
    );
  });

  it("refuses a colour token only :root holds, but not --radius", () => {
    assert.deepEqual(
      cssRules((css) => css.replace("--radius: 0.625rem;", "--radius: 0.625rem;\n  --x: #000;")),
      ["tokens"],
    );
  });

  it("refuses a --color-* that maps to no token", () => {
    assert.deepEqual(
      cssRules((css) =>
        css.replace("--color-background: var(--background);", "--color-x: var(--y);"),
      ),
      ["tokens"],
    );
  });

  it("refuses a media block that differs from .dark", () => {
    assert.deepEqual(
      cssRules((css) =>
        css.replace("color-scheme: dark;\n    --background: #0a0a0a;", "--background: #111111;"),
      ),
      ["media block"],
    );
  });

  it("refuses losing the palette's reset", () => {
    assert.deepEqual(
      cssRules((css) => css.replace("@theme { --color-*: initial; }\n", "")),
      ["palette reset"],
    );
  });

  it("refuses a QR background that differs between the themes", () => {
    assert.deepEqual(
      cssRules((css) =>
        css.replace("--qr-background: #fff;\n}\n@media", "--qr-background: #000;\n}\n@media"),
      ),
      ["tokens"],
    );
  });

  it("ignores comments", () => {
    assert.deepEqual(checkStylesheet(stylesheet((css) => `/* color: red; #fff */\n${css}`)), []);
  });
});

describe("native selects", () => {
  const LOOK =
    'const LOOK = "bg-background text-foreground [&_option]:bg-background [&_option]:text-foreground";';
  const shared = (
    source = `${LOOK}\nexport const S = (p) => <select {...p} className={LOOK} />;`,
  ) => ({
    path: NATIVE_SELECT,
    source,
  });
  const other = (source) => ({ path: "packages/ui/src/features/a/A.tsx", source });
  const matches = (files) => checkSelects(files).map((problem) => problem.match);

  it("takes the shared component alone", () => {
    assert.deepEqual(checkSelects([shared(), other("<NativeSelect id={id} />")]), []);
  });

  it("refuses a select anywhere else, saying where", () => {
    assert.deepEqual(checkSelects([shared(), other('\n<select className="a" />')]), [
      {
        file: "packages/ui/src/features/a/A.tsx",
        line: 2,
        rule: "native select",
        match: "<select: use NativeSelect",
      },
    ]);
  });

  it("refuses one written with createElement", () => {
    assert.equal(checkSelects([shared(), other('createElement("select", {});')]).length, 1);
  });

  for (const [name, source] of [
    ["a line comment", "// a <select here\nconst a = 1;"],
    ["a block comment", "/* <select> */ const a = 1;"],
    ["a string", 'const a = "<select>";'],
    ["a template", "const a = `<select>`;"],
    ["another tag that starts the same", "<selection /> ; <select-all />"],
  ]) {
    it(`doesn't take ${name} for a select`, () => {
      assert.deepEqual(checkSelects([shared(), other(source)]), []);
    });
  }

  it("refuses a shared file that's gone", () => {
    assert.deepEqual(matches([other("const a = 1;")]), ["the file is missing"]);
  });

  it("refuses a shared file with no select, or two", () => {
    assert.deepEqual(matches([shared(LOOK)]), ["it must render exactly one <select"]);
    assert.deepEqual(matches([shared(`${LOOK}\n<select /> ; <select />`)]), [
      "it must render exactly one <select",
    ]);
  });

  for (const token of [
    "bg-background",
    "text-foreground",
    "[&_option]:bg-background",
    "[&_option]:text-foreground",
  ]) {
    it(`refuses a shared look without ${token}`, () => {
      const source = shared().source.replace(`"${token} `, '"').replace(` ${token}`, "");
      assert.ok(matches([shared(source)]).includes(`it must hold ${token}`), source);
    });
  }

  for (const see of ["bg-transparent", "bg-background/50", "bg-input/30"]) {
    it(`refuses ${see} on the shared look`, () => {
      const source = shared().source.replace('"bg-background ', `"bg-background ${see} `);
      assert.equal(checkSelects([shared(source)])[0]?.match.split(":")[0], see);
    });
  }
});

describe("the real tree", () => {
  const root = fileURLToPath(new URL("..", import.meta.url));

  it("reads the UI's sources, the QR code's among them", () => {
    const files = sourceFiles(root);
    assert.ok(files.includes("packages/ui/src/features/auth/components/SetupStep.tsx"));
    assert.ok(files.includes("apps/desktop/src/main.tsx"));
    assert.ok(!files.some((file) => file.includes(".test.")));
  });

  it("finds all of the token blocks", () => {
    const css = readFileSync(`${root}packages/ui/src/styles.css`, "utf8");
    for (const block of [":root {", ".dark {", "@theme inline {", "--color-*: initial"]) {
      assert.ok(css.includes(block), block);
    }
  });

  it("holds no hard-coded colour", () => {
    assert.deepEqual(checkRepository(root), []);
  });
});
