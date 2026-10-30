// A mode as the editor's form holds it (Plan.md P11.9; ADR-0020, S12, S82, S84, U68, U70, U72,
// R5, R158–R160). Pure: no React, no client.
//
// - **The values** are three lists of steps, one per section, so a step can't change section by
//   construction. Every field is a string, what the input shows, so `isDirty` compares what was
//   loaded with what's on the screen. Only what a user may write is here: never
//   `arguments_hidden`, `unlisted_command` or a flag, so no answer can be echoed (R5).
// - **The request** is built from the values alone: the sections in order, each in its shown
//   order, and only the fields a step's kind takes. An empty number is left out, for the server's
//   "is required for this type", but for the chance, whose absence is a bare 400 (S82). A number
//   the wire can't carry is sent to `validated()` as it is, which refuses it at its field.
// - **The paths** say where each request path's error shows, through the order sent.
import type { Path } from "react-hook-form";

import type { ModeActionKind } from "../../generated/ModeActionKind";
import type { ModeActionRequest } from "../../generated/ModeActionRequest";
import type { ModeResponse } from "../../generated/ModeResponse";
import type { ModeSchedule } from "../../generated/ModeSchedule";
import type { ModeScheduleRequest } from "../../generated/ModeScheduleRequest";
import type { ModeStep } from "../../generated/ModeStep";
import type { ModeStepRequest } from "../../generated/ModeStepRequest";
import type { ModeVisibility } from "../../generated/ModeVisibility";
import { ACTION_TEXT, copyName } from "./texts";

/** The sections, in the order a save sends them. */
export const SECTIONS = ["atStart", "repeating", "onRespawn"] as const;

/** One of a mode's sections (U68). */
export type Section = (typeof SECTIONS)[number];

/** Every action this version knows, in the "What it does" list's order. */
export const ACTION_KINDS = Object.keys(ACTION_TEXT) as readonly ModeActionKind[];

/** A time field's units, largest last (U72). */
export const TIME_UNITS = ["seconds", "minutes", "hours"] as const;

/** A time field's unit. */
export type TimeUnit = (typeof TIME_UNITS)[number];

/** The milliseconds of each unit. */
const UNIT_MS: Readonly<Record<TimeUnit, number>> = {
  seconds: 1_000,
  minutes: 60_000,
  hours: 3_600_000,
};

/** The hotbar's keys, as players know them (the user's note at group C's plan review). */
export const SLOT_KEYS = ["1", "2", "3", "4", "5", "6", "7", "8", "9"] as const;

/** One step on the form. */
export interface StepValues {
  kind: ModeActionKind;
  yaw: string;
  pitch: string;
  maxYaw: string;
  maxPitch: string;
  /** Whether `sneak` or `hold_use` starts or stops. */
  on: "start" | "stop";
  /** The key, 1 to 9. */
  slot: string;
  message: string;
  every: string;
  everyUnit: TimeUnit;
  jitter: string;
  jitterUnit: TimeUnit;
  chance: string;
  /** The command as it came cut to its name (S15), for its mark outside the text (R170). */
  cutCommand: string | null;
}

/** The form: what a request is built from. */
export interface ModeValues {
  name: string;
  visibility: ModeVisibility;
  atStart: StepValues[];
  repeating: StepValues[];
  onRespawn: StepValues[];
}

/** A new step: a jump, always, without a wait yet. */
export function newStep(): StepValues {
  return {
    kind: "jump",
    yaw: "",
    pitch: "",
    maxYaw: "",
    maxPitch: "",
    on: "start",
    slot: "1",
    message: "",
    every: "",
    everyUnit: "seconds",
    jitter: "",
    jitterUnit: "seconds",
    chance: "100",
    cutCommand: null,
  };
}

/** A new mode's values; every new mode starts private (U67). */
export function newValues(): ModeValues {
  return { name: "", visibility: "private", atStart: [], repeating: [], onRespawn: [] };
}

/** Whether this version knows action kind `kind`. */
function knownAction(kind: ModeActionKind): boolean {
  return Object.hasOwn(ACTION_TEXT, kind);
}

/** The section of a step with `schedule`, or nothing for a schedule this version doesn't know. */
export function sectionOf(schedule: Pick<ModeSchedule, "type">): Section | null {
  switch (schedule.type) {
    case "at_start":
      return "atStart";
    case "every":
      return "repeating";
    case "on_respawn":
      return "onRespawn";
    default:
      return null;
  }
}

/** Whether the form can hold `step`: its action and its schedule are known. */
export function isKnownStep(step: ModeStep): boolean {
  return knownAction(step.action.type) && sectionOf(step.schedule) !== null;
}

/** Whether every part of `mode` is one this version knows, its visibility included. */
export function isKnownMode(mode: ModeResponse): boolean {
  return (
    (mode.visibility === "private" || mode.visibility === "shared") &&
    mode.steps.every((step) => isKnownStep(step))
  );
}

/**
 * Whether `mode` opens in an editable form: the server's flag, every part known, and no command
 * cut, so no request can ever start from a cut step (R160, S83).
 */
export function isEditable(mode: ModeResponse): boolean {
  return (
    mode.editable &&
    isKnownMode(mode) &&
    mode.steps.every((step) => step.action.arguments_hidden !== true)
  );
}

/** `ms` as its largest unit that shows it whole, seconds otherwise (U72). */
export function timeOf(ms: number | undefined): { readonly text: string; readonly unit: TimeUnit } {
  if (ms === undefined) {
    return { text: "", unit: "seconds" };
  }
  const whole = [...TIME_UNITS].reverse().find((unit) => ms > 0 && ms % UNIT_MS[unit] === 0);
  const unit = whole ?? "seconds";
  return { text: String(ms / UNIT_MS[unit]), unit };
}

/** `value` as its field's text; nothing as an empty field. */
function textOf(value: number | undefined): string {
  return value === undefined ? "" : String(value);
}

/** `step`'s values, which the form can hold (`isKnownStep`). */
function stepValues(step: ModeStep): StepValues {
  const { action, schedule } = step;
  const every = timeOf(schedule.interval_ms);
  const jitter = timeOf(schedule.jitter_ms);
  return {
    kind: action.type,
    yaw: textOf(action.yaw),
    pitch: textOf(action.pitch),
    maxYaw: textOf(action.max_yaw),
    maxPitch: textOf(action.max_pitch),
    on: action.on === false ? "stop" : "start",
    slot: String((action.slot ?? 0) + 1),
    message: action.message ?? "",
    every: every.text,
    everyUnit: every.unit,
    jitter: jitter.text,
    jitterUnit: jitter.unit,
    chance: String(step.probability),
    cutCommand: action.arguments_hidden ? (action.message ?? "") : null,
  };
}

/** `mode`'s values, each section's steps in their stored order; steps it can't hold are left out. */
export function valuesOf(mode: ModeResponse): ModeValues {
  const values: ModeValues = {
    name: mode.name,
    visibility: mode.visibility,
    atStart: [],
    repeating: [],
    onRespawn: [],
  };
  for (const step of mode.steps) {
    const section = sectionOf(step.schedule);
    if (section !== null && knownAction(step.action.type)) {
      values[section].push(stepValues(step));
    }
  }
  return values;
}

/** What a copy of a form holding `values` starts from: the suggested name, and private (U63, U67). */
export function copyOfValues(values: ModeValues): ModeValues {
  return { ...values, name: copyName(values.name), visibility: "private" };
}

/** What a copy of `mode` starts from. */
export function copyValues(mode: ModeResponse): ModeValues {
  return copyOfValues(valuesOf(mode));
}

/** The steps of `mode` the form can't hold, by the section they belong in, if any. */
export function unknownSteps(mode: ModeResponse): readonly (Section | null)[] {
  return mode.steps.filter((step) => !isKnownStep(step)).map((step) => sectionOf(step.schedule));
}

/** `text` as a number; an empty field as nothing. */
function numberOf(text: string): number | undefined {
  return text.trim() === "" ? undefined : Number(text);
}

/** How many steps of a double's own precision count as binary noise, not as what was typed. */
const NOISE_STEPS = 4;

/**
 * `text` in `unit` as milliseconds; nothing for an empty field. Only binary fractions' noise is
 * rounded away, relative to the value, so one that isn't whole milliseconds stays so and is
 * refused (S82).
 */
function millis(text: string, unit: TimeUnit): number | undefined {
  const value = numberOf(text);
  if (value === undefined) {
    return undefined;
  }
  const ms = value * UNIT_MS[unit];
  const near = Math.round(ms);
  return Math.abs(ms - near) <= NOISE_STEPS * Number.EPSILON * Math.abs(ms) ? near : ms;
}

/** `key` set to `value`, or nothing without a value: a request's field left out. */
function only<K extends string>(key: K, value: number | undefined): { [P in K]?: number } {
  return (value === undefined ? {} : { [key]: value }) as { [P in K]?: number };
}

/** `step`'s action: only the fields its kind takes (S12). */
function actionOf(step: StepValues): ModeActionRequest {
  const type = step.kind;
  switch (type) {
    case "look":
      return { type, ...only("yaw", numberOf(step.yaw)), ...only("pitch", numberOf(step.pitch)) };
    case "rotate_random":
      return {
        type,
        ...only("max_yaw", numberOf(step.maxYaw)),
        ...only("max_pitch", numberOf(step.maxPitch)),
      };
    case "sneak":
    case "hold_use":
      return { type, on: step.on === "start" };
    case "select_hotbar_slot":
      return { type, slot: Number(step.slot) - 1 };
    case "send_chat":
      return { type, message: step.message };
    default:
      return { type };
  }
}

/** The schedule of `step` in `section`: only the fields its type takes (S12). */
function scheduleOf(section: Section, step: StepValues): ModeScheduleRequest {
  switch (section) {
    case "atStart":
      return { type: "at_start" };
    case "onRespawn":
      return { type: "on_respawn" };
    default:
      return {
        type: "every",
        ...only("interval_ms", millis(step.every, step.everyUnit)),
        ...only("jitter_ms", millis(step.jitter, step.jitterUnit)),
      };
  }
}

/** A chance: `NaN` for an empty field, since a missing one is a bare 400 (S82). */
function chanceOf(text: string): number {
  return numberOf(text) ?? Number.NaN;
}

/** What a save sends, and where each path of its errors shows. */
export interface BuiltSteps {
  readonly steps: ModeStepRequest[];
  readonly paths: Readonly<Record<string, Path<ModeValues>>>;
}

/** The request's steps from `values` alone, and their paths; see the module comment. */
export function stepsOf(values: ModeValues): BuiltSteps {
  const steps: ModeStepRequest[] = [];
  const paths: Record<string, Path<ModeValues>> = { name: "name", visibility: "visibility" };
  for (const section of SECTIONS) {
    values[section].forEach((step, index) => {
      const at = `steps[${steps.length}]`;
      const field = <K extends keyof StepValues>(name: K) =>
        `${section}.${index}.${name}` as Path<ModeValues>;
      // A rule about the whole step shows on its "What it does" (R158).
      paths[at] = field("kind");
      paths[`${at}.action`] = field("kind");
      paths[`${at}.action.type`] = field("kind");
      paths[`${at}.action.yaw`] = field("yaw");
      paths[`${at}.action.pitch`] = field("pitch");
      paths[`${at}.action.max_yaw`] = field("maxYaw");
      paths[`${at}.action.max_pitch`] = field("maxPitch");
      paths[`${at}.action.on`] = field("on");
      paths[`${at}.action.slot`] = field("slot");
      paths[`${at}.action.message`] = field("message");
      paths[`${at}.schedule.interval_ms`] = field("every");
      paths[`${at}.schedule.jitter_ms`] = field("jitter");
      paths[`${at}.probability`] = field("chance");
      steps.push({
        action: actionOf(step),
        schedule: scheduleOf(section, step),
        probability: chanceOf(step.chance),
      });
    });
  }
  return { steps, paths };
}

/** How many steps `values` holds, over its sections. */
export function stepTotal(values: Pick<ModeValues, Section>): number {
  return SECTIONS.reduce((sum, section) => sum + values[section].length, 0);
}
