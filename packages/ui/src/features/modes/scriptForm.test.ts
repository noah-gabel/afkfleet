import { describe, expect, it } from "vitest";
import type { ModeScript } from "../../generated/ModeScript";
import { SCRIPT_API } from "../../generated/script-api";
import {
  declaredOf,
  isKnownScript,
  newScriptValues,
  type ScriptValues,
  scriptRequestOf,
  scriptValuesOf,
} from "./scriptForm";

/** The balance example's body, as the generated description holds it. */
const BALANCE = SCRIPT_API.examples.find((example) => example.id === "balance")
  ?.script as ModeScript;

describe("a scripted mode's form", () => {
  it("starts a new script with an empty on_join, the default leash and tool guard, and nothing declared", () => {
    const values = newScriptValues();

    expect(values.source).toContain("fn on_join(first)");
    expect([values.leash, values.toolGuard]).toEqual(["8", "2"]);
    expect([values.blocks, values.messages, values.patterns]).toEqual([[], [], []]);
  });

  it("holds a mode's body as its fields show it, each number as text and each word list as one line", () => {
    const values = scriptValuesOf(BALANCE);

    expect(values.source).toBe(BALANCE.source);
    expect(values.messages).toEqual([
      { key: "balance", text: "/balance", values: [] },
      {
        key: "pay",
        text: "/pay AfkMain {amount}",
        values: [
          { name: "amount", type: "decimal", min: "1", max: "1000000", places: "2", words: "" },
        ],
      },
    ]);
    expect(values.patterns).toEqual([
      {
        key: "balance",
        when: "reply",
        text: BALANCE.patterns[0]?.text,
        captures: [
          {
            name: "amount",
            type: "amount",
            min: "0",
            max: "1000000000",
            decimalMark: ".",
            words: "",
          },
        ],
      },
    ]);
  });

  it("builds the request from the fields alone: each value with its type's fields and no others", () => {
    const values: ScriptValues = {
      ...newScriptValues(),
      leash: "12",
      toolGuard: "0",
      blocks: [{ block: "cobblestone" }, { block: " minecraft:stone " }],
      messages: [
        {
          key: "give",
          text: "/give AfkMain {item} {n}",
          values: [
            {
              name: "item",
              type: "word",
              min: "5",
              max: "",
              places: "",
              words: " dirt  stone\tsand ",
            },
            { name: "n", type: "integer", min: "1", max: "64", places: "3", words: "x" },
          ],
        },
      ],
      patterns: [
        {
          key: "restart",
          when: "any_time",
          text: "Restarting in {m} minutes",
          captures: [
            { name: "m", type: "integer", min: "0", max: "60", decimalMark: ",", words: "" },
            { name: "c", type: "amount", min: "", max: "1e9", decimalMark: ",", words: "a" },
          ],
        },
      ],
      signs: [
        {
          key: "price",
          lines: ["{price}", "", "afk", " "],
          values: [{ name: "price", type: "integer", min: "1", max: "100", places: "", words: "" }],
        },
      ],
    };

    const { script } = scriptRequestOf(values);

    expect(script).toEqual({
      source: values.source,
      leash: 12,
      tool_guard: 0,
      block_guard: ["cobblestone", "minecraft:stone"],
      messages: [
        {
          key: "give",
          text: "/give AfkMain {item} {n}",
          values: [
            { name: "item", type: "word", options: ["dirt", "stone", "sand"] },
            { name: "n", type: "integer", min: 1, max: 64 },
          ],
        },
      ],
      patterns: [
        {
          key: "restart",
          when: "any_time",
          text: "Restarting in {m} minutes",
          captures: [
            { name: "m", type: "integer", min: 0, max: 60 },
            { name: "c", type: "amount", max: 1e9, decimal_mark: "," },
          ],
        },
      ],
      // The empty lines at its end left out; one in the middle kept.
      signs: [
        {
          key: "price",
          lines: ["{price}", "", "afk"],
          values: [{ name: "price", type: "integer", min: 1, max: 100 }],
        },
      ],
    });
  });

  it("sends an empty leash or tool guard as no number, for the local check to refuse at its field", () => {
    const { script } = scriptRequestOf({ ...newScriptValues(), leash: " ", toolGuard: "" });

    expect(Number.isNaN(script.leash)).toBe(true);
    expect(Number.isNaN(script.tool_guard)).toBe(true);
  });

  it("reads a body back as itself through its fields", () => {
    for (const { id, script } of SCRIPT_API.examples) {
      const request = scriptRequestOf(scriptValuesOf(script)).script;
      expect(request, id).toEqual({
        source: script.source,
        leash: script.leash,
        tool_guard: script.tool_guard,
        block_guard: script.block_guard,
        messages: script.messages.map(({ key, text, values }) => ({ key, text, values })),
        patterns: script.patterns,
        signs: script.signs,
      });
    }
  });

  it("puts each of the server's paths on its field, and a whole value's rule on its smallest end", () => {
    const values: ScriptValues = {
      ...scriptValuesOf(BALANCE),
      blocks: [{ block: "cobblestone" }],
    };

    const { paths } = scriptRequestOf(values);

    expect(paths).toMatchObject({
      "script.leash": "script.leash",
      "script.tool_guard": "script.toolGuard",
      "script.block_guard[0]": "script.blocks.0.block",
      "script.messages[1].key": "script.messages.1.key",
      "script.messages[1].text": "script.messages.1.text",
      "script.messages[1].values[0]": "script.messages.1.values.0.min",
      "script.messages[1].values[0].name": "script.messages.1.values.0.name",
      "script.messages[1].values[0].type": "script.messages.1.values.0.type",
      "script.messages[1].values[0].places": "script.messages.1.values.0.places",
      "script.messages[1].values[0].options": "script.messages.1.values.0.words",
      "script.patterns[0].when": "script.patterns.0.when",
      "script.patterns[0].captures[0]": "script.patterns.0.captures.0.min",
      "script.patterns[0].captures[0].decimal_mark": "script.patterns.0.captures.0.decimalMark",
    });
    expect(paths["script.source"]).toBeUndefined();
  });

  it("knows a body whose every type this version knows, and no other", () => {
    expect(isKnownScript(BALANCE)).toBe(true);
    const odd = (change: object) => ({ ...BALANCE, ...change }) as ModeScript;
    expect(isKnownScript(odd({ patterns: [{ ...BALANCE.patterns[0], when: "sometimes" }] }))).toBe(
      false,
    );
    expect(
      isKnownScript(
        odd({
          messages: [
            {
              key: "a",
              text: "/a {v}",
              values: [{ name: "v", type: "colour" }],
              arguments_hidden: false,
            },
          ],
        }),
      ),
    ).toBe(false);
  });

  it("says what a script may send and expect, for the editor's marks", () => {
    expect(declaredOf(scriptValuesOf(BALANCE))).toEqual({
      signs: [],
      messages: ["balance", "pay"],
      replies: ["balance"],
    });
  });
});
