// The editor's pure parts: places, the checks the compiler can't make, completion and hover
// help (Plan.md P15.25; ADR-0045). Every name and summary is the generated description's.
import { CompletionContext, type CompletionResult } from "@codemirror/autocomplete";
import { EditorState, Text } from "@codemirror/state";
import { describe, expect, it } from "vitest";

import { SCRIPT_API } from "../../generated/script-api";
import { HOOK_MEMBERS, objectNamed, resolveChain } from "./api";
import { scriptCompletions } from "./complete";
import { helpAt } from "./hover";
import { memberProblems } from "./members";
import { markAt, offsetOf, placeOf } from "./places";
import { rhaiLanguage } from "./rhai";

/** The text of `lines` joined. */
const doc = (...lines: string[]) => Text.of(lines);

describe("places", () => {
  it("counts a column in characters, as the engine does, a tab and an emoji one each", () => {
    const text = doc("fn on_join(first) {", "\t😀 kees.x", "}");

    const at = offsetOf(text, 2, 4);

    expect(text.sliceString(at, at + 4)).toBe("kees");
    expect(placeOf(text, at)).toEqual({ line: 2, column: 4 });
  });

  it("lands a place past a line's end at its end, and a line past the text at its last line", () => {
    const text = doc("ab", "cd");

    expect(offsetOf(text, 1, 99)).toBe(2);
    expect(offsetOf(text, 9, 1)).toBe(3);
    expect(offsetOf(text, 0)).toBe(0);
  });

  it("marks the name at a place, else one character, else the one before the line's end", () => {
    const text = doc("let kees = 1;", "");

    expect(markAt(text, 4)).toEqual({ from: 4, to: 8 });
    expect(markAt(text, 9)).toEqual({ from: 9, to: 10 });
    expect(markAt(text, 13)).toEqual({ from: 12, to: 13 });
    expect(markAt(text, 14)).toEqual({ from: 14, to: 14 });
  });
});

describe("the checks the compiler can't make", () => {
  const messages = (text: string, declared?: Parameters<typeof memberProblems>[1]) =>
    memberProblems(text, declared).map((problem) => [
      text.slice(problem.from, problem.to),
      problem.message,
    ]);

  it("finds a member a game object or an input doesn't have", () => {
    expect(messages("fn on_join(first) { keys.forwrd.hold(); mouse.left.hodl(); }")).toEqual([
      ["forwrd", "keys has no forwrd: the script would stop on this line."],
      ["hodl", "mouse.left has no hodl: the script would stop on this line."],
    ]);
  });

  it("finds a method without its parentheses and a value with them", () => {
    expect(messages("fn on_stop() { keys.release_all; let h = bot.health(); }")).toEqual([
      ["release_all", "keys.release_all is a method: write release_all()."],
      ["health", "bot.health is a value, not a method: leave out the parentheses."],
    ]);
  });

  it("passes every member the description has, and stops at a value", () => {
    const text = [
      "fn on_join(first) {",
      "    keys.forward.hold_for(500);",
      '    let w = world.nearest("minecart").x;',
      "    if keys.sneak.held { look.at(90, 10.5); }",
      "    state.keys = bot.held;",
      "    // keys.nothing in a comment",
      `    log.info(\`keys.nothing \${bot.x}\`);`,
      "}",
    ].join("\n");

    expect(memberProblems(text)).toEqual([]);
  });

  it("finds a declared key chat names that the mode doesn't declare, only when it knows them", () => {
    const text =
      'fn on_join(first) { chat.send("pay2"); chat.expect("balance", 5000); chat.send(key); }';

    expect(messages(text, { messages: ["pay"], replies: [] })).toEqual([
      ['"pay2"', 'The mode declares no message "pay2": the script would stop on this line.'],
      [
        '"balance"',
        'The mode declares no reply pattern "balance": the script would stop on this line.',
      ],
    ]);
    expect(messages(text, { messages: ["pay2"], replies: ["balance"] })).toEqual([]);
    expect(messages(text)).toEqual([]);
  });

  it("finds a sign sign.write names that the mode doesn't declare, only when it knows its signs", () => {
    const text = 'fn on_sign_open() { sign.write("prise", #{}); sign.write(key, #{}); }';

    expect(messages(text, { signs: ["price"], messages: [], replies: [] })).toEqual([
      ['"prise"', 'The mode declares no sign "prise": the script would stop on this line.'],
    ]);
    expect(messages(text, { signs: ["prise"], messages: [], replies: [] })).toEqual([]);
    expect(messages(text, { messages: [], replies: [] })).toEqual([]);
  });

  it("passes every example of the reference with its mode's declarations", () => {
    for (const { id, script } of SCRIPT_API.examples) {
      const declared = {
        signs: script.signs.map((sign) => sign.key),
        messages: script.messages.map((message) => message.key),
        replies: script.patterns.filter((one) => one.when === "reply").map((one) => one.key),
      };
      expect(memberProblems(script.source, declared), id).toEqual([]);
    }
  });
});

/** The completions at the `|` in `text`. */
function completionsAt(text: string, explicit = false): CompletionResult | null {
  const pos = text.indexOf("|");
  const state = EditorState.create({
    doc: text.replace("|", ""),
    extensions: [rhaiLanguage],
  });
  return scriptCompletions(new CompletionContext(state, pos, explicit));
}

/** The labels offered at the `|` in `text`. */
function labelsAt(text: string, explicit = false): string[] | null {
  return completionsAt(text, explicit)?.options.map((option) => option.label) ?? null;
}

describe("completion", () => {
  it("offers a game object's members after its dot, a method's signature beside it, each one's summary", () => {
    const result = completionsAt("fn on_join(first) { look.| }");
    const look = objectNamed("look");

    expect(result?.options.map((option) => [option.label, option.detail, option.info])).toEqual(
      look?.members.map((member) => [
        member.name,
        member.kind === "method" ? member.signature : undefined,
        member.summary,
      ]),
    );
  });

  it("offers an input's members after a key's or a button's dot, as the description chains them", () => {
    expect(labelsAt("fn on_join(first) { keys.forward.ho| }")).toEqual(
      resolveChain(["keys", "forward"])?.members.map((member) => member.name),
    );
    expect(labelsAt("fn on_join(first) { mouse.left.| }")).toContain("hold_until_used");
  });

  it("offers nothing after a value's dot or inside a comment or a text", () => {
    expect(labelsAt("fn on_join(first) { bot.health.| }")).toBeNull();
    expect(labelsAt("fn on_join(first) { // keys.| }")).toBeNull();
    expect(labelsAt('fn on_join(first) { log.info("keys.|") }')).toBeNull();
  });

  it("offers every hook with its parameters and an empty body for on_ at a line's start", () => {
    const result = completionsAt("fn on_stop() { }\non_|");

    expect(result?.options.map((option) => [option.label, option.detail])).toEqual(
      HOOK_MEMBERS.map((hook) => [hook.name, hook.signature]),
    );
  });

  it("offers the game objects for a name, or when asked: never a map a script reads", () => {
    const names = SCRIPT_API.objects
      .filter((object) => object.kind === "game")
      .map((object) => object.name);

    expect(labelsAt("fn on_join(first) { k| }")).toEqual(names);
    expect(labelsAt("fn on_join(first) { | }", true)).toEqual(names);
    expect(labelsAt("fn on_join(first) { | }")).toBeNull();
    expect(names).toEqual(expect.arrayContaining(["inventory", "menu", "sign", "clock"]));
    for (const value of ["slot", "time", "input", "hooks"]) {
      expect(names).not.toContain(value);
    }
  });

  it("offers a map's keys after a call that answers one, as the description says (Plan.md P16.16)", () => {
    const slot = objectNamed("slot")?.members.map((member) => member.name);
    const time = objectNamed("time")?.members.map((member) => member.name);

    expect(labelsAt("fn on_menu_open() { menu.slot(13).| }")).toEqual(slot);
    expect(labelsAt("fn on_menu_open() { menu.at(menu.rows - 1, 4).| }")).toEqual(slot);
    expect(labelsAt('fn on_join(first) { inventory.find("minecraft:potion").co| }')).toEqual(slot);
    expect(labelsAt('fn on_join(first) { clock.local("Europe/Berlin").| }')).toEqual(time);
    expect(time).toEqual(expect.arrayContaining(["hour", "minute", "weekday"]));
    expect(labelsAt("fn on_join(first) { bot.name.| }")).toBeNull();
    expect(labelsAt("fn on_join(first) { menu.close().| }")).toBeNull();
  });

  it("knows level 2's names: bot.name, the menu's, the sign's and the five hooks", () => {
    expect(labelsAt("fn on_join(first) { bot.| }")).toContain("name");
    expect(labelsAt("fn on_join(first) { sign.| }")).toContain("write");
    expect(labelsAt("fn on_menu_close() { }\non_|")).toEqual(
      expect.arrayContaining(["on_menu_open", "on_menu_update", "on_click", "on_sign_open"]),
    );
  });
});

describe("hover help", () => {
  const text = "fn on_chat(key, values) {\n    keys.forward.hold_for(500);\n    bot.x;\n}";
  const at = (word: string, nth = 0) => {
    let index = -1;
    for (let found = 0; found <= nth; found += 1) {
      index = text.indexOf(word, index + 1);
    }
    return index + 1;
  };

  it("shows a member's signature from its game object, and its summary", () => {
    const hold = objectNamed("input")?.members.find((member) => member.name === "hold_for");

    expect(helpAt(text, at("hold_for"))).toMatchObject({
      signature: "keys.forward.hold_for(ms)",
      summary: hold?.summary,
    });
    expect(helpAt(text, at("forward"))?.signature).toBe("keys.forward");
  });

  it("shows a game object's summary and a hook's definition", () => {
    expect(helpAt(text, at("bot"))).toMatchObject({
      signature: "bot",
      summary: objectNamed("bot")?.summary,
    });
    expect(helpAt(text, at("on_chat"))?.signature).toBe("fn on_chat(key, values)");
  });

  it("shows nothing over a plain name or a space", () => {
    expect(helpAt(text, at("values"))).toBeNull();
    expect(helpAt(text, 0)).toBeNull();
  });
});
