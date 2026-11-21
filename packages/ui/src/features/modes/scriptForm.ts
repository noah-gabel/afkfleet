// A scripted mode's body as the editor's form holds it (Plan.md P15.25, P15.35; ADR-0036, D8,
// D10, D11; ADR-0037, C3, C5; ADR-0042, G13; ADR-0045). Pure: no React, no client.
//
// - **The values** are what the inputs show: every number as its text, a list of words as one
//   line, the blocks a held left button mines as a list. Only what a user may write is here: never
//   `api`, `any_time_chat` or `arguments_hidden`, so no answer is echoed (R5).
// - **The request** is built from the values alone: each declared value and capture with the
//   fields its type takes and no others, an empty number left out for the server's "is needed for
//   this type", an empty leash or tool guard as no number, which the local check refuses at its
//   field (S82). Every rule is the server's.
// - **The paths** put each of the server's paths on its field; a rule about a whole value or
//   capture (its range) shows on its smallest end. `script.source` has none: the editor shows it
//   at its line.
import type { Path } from "react-hook-form";

import type { ModeCaptureKind } from "../../generated/ModeCaptureKind";
import type { ModeCaptureRequest } from "../../generated/ModeCaptureRequest";
import type { ModeDecimalMark } from "../../generated/ModeDecimalMark";
import type { ModeScript } from "../../generated/ModeScript";
import type { ModeScriptRequest } from "../../generated/ModeScriptRequest";
import type { ModeValueKind } from "../../generated/ModeValueKind";
import type { ModeValueRequest } from "../../generated/ModeValueRequest";
import type { PatternWhen } from "../../generated/PatternWhen";
import type { Declared } from "../scripts/members";
import type { ModeValues } from "./modeForm";

/** A declared value on the form. */
export interface ValueValues {
  name: string;
  type: ModeValueKind;
  min: string;
  max: string;
  places: string;
  /** A word list's words, separated by spaces. */
  words: string;
}

/** A declared message on the form. */
export interface MessageValues {
  key: string;
  text: string;
  values: ValueValues[];
}

/** A pattern's capture on the form. */
export interface CaptureValues {
  name: string;
  type: ModeCaptureKind;
  min: string;
  max: string;
  decimalMark: ModeDecimalMark;
  /** A word list's words, separated by spaces. */
  words: string;
}

/** A declared chat pattern on the form. */
export interface PatternValues {
  key: string;
  when: PatternWhen;
  text: string;
  captures: CaptureValues[];
}

/** A scripted mode's body on the form. */
export interface ScriptValues {
  source: string;
  leash: string;
  toolGuard: string;
  blocks: { block: string }[];
  messages: MessageValues[];
  patterns: PatternValues[];
}

/** Every value type, capture type and `when` this version knows (S12), from the generated types. */
const VALUE_KINDS: { readonly [K in ModeValueKind]: K } = {
  integer: "integer",
  decimal: "decimal",
  word: "word",
};
const CAPTURE_KINDS: { readonly [K in ModeCaptureKind]: K } = {
  integer: "integer",
  amount: "amount",
  word: "word",
};
const WHENS: { readonly [K in PatternWhen]: K } = { reply: "reply", any_time: "any_time" };
const MARKS: { readonly [K in ModeDecimalMark]: K } = { ".": ".", ",": "," };

/** The value types, capture types and `when`s, in their lists' order. */
export const VALUE_TYPES = Object.values(VALUE_KINDS);
export const CAPTURE_TYPES = Object.values(CAPTURE_KINDS);
export const PATTERN_WHENS = Object.values(WHENS);

/** A new script's text: an empty hook to start from. */
const NEW_SOURCE = "fn on_join(first) {\n    \n}\n";

/** A new scripted mode's body: the default leash and tool guard (ADR-0036, D10, D11), nothing declared. */
export function newScriptValues(): ScriptValues {
  return {
    source: NEW_SOURCE,
    leash: "8",
    toolGuard: "2",
    blocks: [],
    messages: [],
    patterns: [],
  };
}

/** A new declared value. */
export function newValue(): ValueValues {
  return { name: "", type: "integer", min: "", max: "", places: "", words: "" };
}

/** A new declared message. */
export function newMessage(): MessageValues {
  return { key: "", text: "", values: [] };
}

/** A new capture. */
export function newCapture(): CaptureValues {
  return { name: "", type: "integer", min: "", max: "", decimalMark: ".", words: "" };
}

/** A new pattern: a reply, the safer kind (ADR-0037, C5). */
export function newPattern(): PatternValues {
  return { key: "", when: "reply", text: "", captures: [] };
}

/** `value` as its field's text; nothing as an empty field. */
function textOf(value: number | undefined): string {
  return value === undefined ? "" : String(value);
}

/** `body`'s values; see the module comment. */
export function scriptValuesOf(body: ModeScript): ScriptValues {
  return {
    source: body.source,
    leash: String(body.leash),
    toolGuard: String(body.tool_guard),
    blocks: body.block_guard.map((block) => ({ block })),
    messages: body.messages.map((message) => ({
      key: message.key,
      text: message.text,
      values: message.values.map((value) => ({
        name: value.name,
        type: value.type,
        min: textOf(value.min),
        max: textOf(value.max),
        places: textOf(value.places),
        words: (value.options ?? []).join(" "),
      })),
    })),
    patterns: body.patterns.map((pattern) => ({
      key: pattern.key,
      when: pattern.when,
      text: pattern.text,
      captures: pattern.captures.map((capture) => ({
        name: capture.name,
        type: capture.type,
        min: textOf(capture.min),
        max: textOf(capture.max),
        decimalMark: capture.decimal_mark ?? ".",
        words: (capture.options ?? []).join(" "),
      })),
    })),
  };
}

/** Whether the form can hold `body`: every type, `when` and mark is one this version knows. */
export function isKnownScript(body: ModeScript): boolean {
  return (
    body.messages.every((message) =>
      message.values.every((value) => Object.hasOwn(VALUE_KINDS, value.type)),
    ) &&
    body.patterns.every(
      (pattern) =>
        Object.hasOwn(WHENS, pattern.when) &&
        pattern.captures.every(
          (capture) =>
            Object.hasOwn(CAPTURE_KINDS, capture.type) &&
            (capture.decimal_mark === undefined || Object.hasOwn(MARKS, capture.decimal_mark)),
        ),
    )
  );
}

/** What the form declares, for the editor's marks of `chat.send` and `chat.expect`. */
export function declaredOf(values: ScriptValues): Declared {
  return {
    messages: values.messages.map((message) => message.key.trim()),
    replies: values.patterns
      .filter((pattern) => pattern.when === "reply")
      .map((pattern) => pattern.key.trim()),
  };
}

/** `text` as a number; an empty field as nothing. */
function numberOf(text: string): number | undefined {
  return text.trim() === "" ? undefined : Number(text);
}

/** A field that can't be left out: an empty one as no number. */
function required(text: string): number {
  return numberOf(text) ?? Number.NaN;
}

/** `text`'s words: what whitespace separates. */
function wordsOf(text: string): string[] {
  return text.split(/\s+/).filter((word) => word !== "");
}

/** `key` set to `value`, or nothing without a value: a request's field left out. */
function only<K extends string, V>(key: K, value: V | undefined): { [P in K]?: V } {
  return (value === undefined ? {} : { [key]: value }) as { [P in K]?: V };
}

/** A declared value's request: the fields its type takes. */
function valueRequest(value: ValueValues): ModeValueRequest {
  const { name, type } = value;
  const range = { ...only("min", numberOf(value.min)), ...only("max", numberOf(value.max)) };
  switch (type) {
    case "integer":
      return { name, type, ...range };
    case "decimal":
      return { name, type, ...range, ...only("places", numberOf(value.places)) };
    default:
      return { name, type, options: wordsOf(value.words) };
  }
}

/** A capture's request: the fields its type takes. */
function captureRequest(capture: CaptureValues): ModeCaptureRequest {
  const { name, type } = capture;
  const range = { ...only("min", numberOf(capture.min)), ...only("max", numberOf(capture.max)) };
  switch (type) {
    case "integer":
      return { name, type, ...range };
    case "amount":
      return { name, type, ...range, decimal_mark: capture.decimalMark };
    default:
      return { name, type, options: wordsOf(capture.words) };
  }
}

/** What a save sends of the body, and where each path of its errors shows. */
export interface BuiltScript {
  readonly script: ModeScriptRequest;
  readonly paths: Readonly<Record<string, Path<ModeValues>>>;
}

/** The request's body from `values` alone, and its paths; see the module comment. */
export function scriptRequestOf(values: ScriptValues): BuiltScript {
  const paths: Record<string, string> = {
    "script.leash": "script.leash",
    "script.tool_guard": "script.toolGuard",
  };
  values.blocks.forEach((_, b) => {
    paths[`script.block_guard[${b}]`] = `script.blocks.${b}.block`;
  });
  values.messages.forEach((message, m) => {
    const at = `script.messages[${m}]`;
    const field = `script.messages.${m}`;
    paths[`${at}.key`] = `${field}.key`;
    paths[`${at}.text`] = `${field}.text`;
    message.values.forEach((_, v) => {
      const value = `${field}.values.${v}`;
      paths[`${at}.values[${v}]`] = `${value}.min`;
      for (const [wire, form] of [
        ["name", "name"],
        ["type", "type"],
        ["min", "min"],
        ["max", "max"],
        ["places", "places"],
        ["options", "words"],
      ]) {
        paths[`${at}.values[${v}].${wire}`] = `${value}.${form}`;
      }
    });
  });
  values.patterns.forEach((pattern, p) => {
    const at = `script.patterns[${p}]`;
    const field = `script.patterns.${p}`;
    paths[`${at}.key`] = `${field}.key`;
    paths[`${at}.when`] = `${field}.when`;
    paths[`${at}.text`] = `${field}.text`;
    pattern.captures.forEach((_, c) => {
      const capture = `${field}.captures.${c}`;
      paths[`${at}.captures[${c}]`] = `${capture}.min`;
      for (const [wire, form] of [
        ["name", "name"],
        ["type", "type"],
        ["min", "min"],
        ["max", "max"],
        ["decimal_mark", "decimalMark"],
        ["options", "words"],
      ]) {
        paths[`${at}.captures[${c}].${wire}`] = `${capture}.${form}`;
      }
    });
  });
  return {
    script: {
      source: values.source,
      leash: required(values.leash),
      tool_guard: required(values.toolGuard),
      block_guard: values.blocks.map(({ block }) => block.trim()),
      messages: values.messages.map((message) => ({
        key: message.key,
        text: message.text,
        values: message.values.map(valueRequest),
      })),
      patterns: values.patterns.map((pattern) => ({
        key: pattern.key,
        when: pattern.when,
        text: pattern.text,
        captures: pattern.captures.map(captureRequest),
      })),
    },
    paths: paths as Record<string, Path<ModeValues>>,
  };
}
