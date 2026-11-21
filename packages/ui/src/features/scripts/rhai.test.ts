import { foldable, getIndentation, indentUnit } from "@codemirror/language";
import { EditorState } from "@codemirror/state";
import { describe, expect, it } from "vitest";

import { EXAMPLES } from "./api";
import { braceFold, rhaiLanguage, type TokenType, tokens } from "./rhai";

/** `text`'s tokens as `[type, text]`. */
function typed(text: string): [TokenType, string][] {
  return tokens(text).map((token) => [token.type, token.text]);
}

describe("the Rhai tokenizer", () => {
  it("tells keywords, game objects, members, hooks' definitions and plain names apart", () => {
    expect(typed("fn on_join(first) { keys.forward.hold(); let n = state.keys; }")).toEqual([
      ["keyword", "fn"],
      ["definition", "on_join"],
      ["bracket", "("],
      ["name", "first"],
      ["bracket", ")"],
      ["bracket", "{"],
      ["object", "keys"],
      ["dot", "."],
      ["member", "forward"],
      ["dot", "."],
      ["member", "hold"],
      ["bracket", "("],
      ["bracket", ")"],
      ["operator", ";"],
      ["keyword", "let"],
      ["name", "n"],
      ["operator", "="],
      ["name", "state"],
      ["dot", "."],
      ["member", "keys"],
      ["operator", ";"],
      ["bracket", "}"],
    ]);
  });

  it("reads numbers as Rhai writes them, and true and false as values", () => {
    expect(typed("1_000 0x1F 0o17 0b101 1.5e-3 true false")).toEqual([
      ["number", "1_000"],
      ["number", "0x1F"],
      ["number", "0o17"],
      ["number", "0b101"],
      ["number", "1.5e-3"],
      ["atom", "true"],
      ["atom", "false"],
    ]);
  });

  it("reads texts, a character and comments, nothing inside them as code", () => {
    expect(
      typed(`"keys.x \\" // no" 'a' '\\n' // keys.forward\n/* a /* nested */ keys */ x`),
    ).toEqual([
      ["string", `"keys.x \\" // no"`],
      ["string", "'a'"],
      ["string", "'\\n'"],
      ["comment", "// keys.forward"],
      ["comment", "/* a /* nested */ keys */"],
      ["name", "x"],
    ]);
  });

  it("carries a text past a line's end after a backslash, and a block comment over lines", () => {
    expect(typed('"one \\\ntwo" x /* a\nkeys */ y')).toEqual([
      ["string", '"one \\'],
      ["string", 'two"'],
      ["name", "x"],
      ["comment", "/* a"],
      ["comment", "keys */"],
      ["name", "y"],
    ]);
  });

  it("reads code inside a backtick text's interpolation and its text around it, over lines", () => {
    expect(typed(`\`a \${bot.x + #{b: 1}.b} c\nd\` e`)).toEqual([
      ["string", "`a "],
      ["bracket", "${"],
      ["object", "bot"],
      ["dot", "."],
      ["member", "x"],
      ["operator", "+"],
      ["bracket", "#{"],
      ["name", "b"],
      ["operator", ":"],
      ["number", "1"],
      ["bracket", "}"],
      ["dot", "."],
      ["member", "b"],
      ["bracket", "}"],
      ["string", " c"],
      ["string", "d`"],
      ["name", "e"],
    ]);
  });

  it("marks a raw string's start as invalid: the compiler refuses one", () => {
    expect(typed('#"raw"#')[0]).toEqual(["invalid", '#"']);
  });

  it("reads every example of the reference with nothing invalid", () => {
    for (const example of EXAMPLES) {
      const all = tokens(example.script.source);
      expect(all.length, example.id).toBeGreaterThan(10);
      expect(
        all.filter((token) => token.type === "invalid"),
        example.id,
      ).toEqual([]);
    }
  });
});

describe("Rhai's indentation and folding", () => {
  const state = (doc: string) =>
    EditorState.create({ doc, extensions: [rhaiLanguage, braceFold, indentUnit.of("    ")] });

  it("indents a line by the brackets open at its start, one less when it closes one", () => {
    const doc = "fn on_join(first) {\nif first {\nx\n}\n}";
    const at = state(doc);
    const indent = (line: number) => getIndentation(at, at.doc.line(line).from);

    expect([indent(2), indent(3), indent(4), indent(5)]).toEqual([4, 8, 4, 0]);
  });

  it("folds a line that ends with { to its closing }", () => {
    const at = state("fn on_join(first) {\n    keys.jump.tap();\n} // end\nfn on_stop() { }");

    expect(foldable(at, 0, at.doc.line(1).to)).toEqual({ from: 19, to: 41 });
    expect(foldable(at, at.doc.line(4).from, at.doc.line(4).to)).toBeNull();
  });
});
