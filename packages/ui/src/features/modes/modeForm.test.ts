import { describe, expect, it } from "vitest";

import type { ModeActionKind } from "../../generated/ModeActionKind";
import type { ModeResponse } from "../../generated/ModeResponse";
import type { ModeScheduleKind } from "../../generated/ModeScheduleKind";
import type { ModeStep } from "../../generated/ModeStep";
import type { ModeStepRequest } from "../../generated/ModeStepRequest";
import type { ModeVisibility } from "../../generated/ModeVisibility";
import { mode } from "../../test/live";
import {
  ACTION_KINDS,
  copyValues,
  isEditable,
  isKnownMode,
  newStep,
  newValues,
  sectionOf,
  stepsOf,
  stepTotal,
  timeOf,
  unknownSteps,
  valuesOf,
} from "./modeForm";

const AT_START = { type: "at_start" } as const;
const EVERY = { type: "every", interval_ms: 45_000, jitter_ms: 0 } as const;
const ON_RESPAWN = { type: "on_respawn" } as const;

/** A step as a request carries it. */
function asked(
  action: ModeStepRequest["action"],
  schedule: ModeStepRequest["schedule"] = AT_START,
  probability = 100,
): ModeStepRequest {
  return { action, schedule, probability };
}

/** `request` as an answer shows it: `arguments_hidden` beside its action's fields. */
function shown(request: ModeStepRequest, hidden = false): ModeStep {
  return { ...request, action: { ...request.action, arguments_hidden: hidden } };
}

/** The mode whose steps are `steps`, in their order, as an editor sees it. */
function holding(...steps: ModeStepRequest[]): ModeResponse {
  return mode(1, { steps: steps.map((step) => shown(step)) });
}

describe("a mode's values and its request (R158, R159)", () => {
  it.each<[string, ModeStepRequest]>([
    ["look", asked({ type: "look", yaw: -12.5, pitch: 30 })],
    ["rotate_random", asked({ type: "rotate_random", max_yaw: 30, max_pitch: 10 })],
    ["jump", asked({ type: "jump" })],
    ["sneak on", asked({ type: "sneak", on: true })],
    ["sneak off", asked({ type: "sneak", on: false })],
    ["swing_arm", asked({ type: "swing_arm" })],
    ["use_item", asked({ type: "use_item" })],
    ["hold_use on", asked({ type: "hold_use", on: true })],
    ["hold_use off", asked({ type: "hold_use", on: false })],
    ["attack_facing_entity", asked({ type: "attack_facing_entity" })],
    ["the first slot", asked({ type: "select_hotbar_slot", slot: 0 })],
    ["the last slot", asked({ type: "select_hotbar_slot", slot: 8 })],
    ["send_chat", asked({ type: "send_chat", message: "/warp farm" })],
    ["a chance", asked({ type: "jump" }, AT_START, 25)],
    ["a repeating step", asked({ type: "jump" }, EVERY)],
    ["a wait and its random part", asked({ type: "jump" }, { ...EVERY, jitter_ms: 75_000 })],
    ["a step on respawn", asked({ type: "jump" }, ON_RESPAWN)],
  ])("sends %s as it came, built from the values alone", (_name, step) => {
    expect(stepsOf(valuesOf(holding(step))).steps).toEqual([step]);
  });

  it("knows every action this version's type holds", () => {
    const every: Record<ModeActionKind, true> = {
      look: true,
      rotate_random: true,
      jump: true,
      sneak: true,
      swing_arm: true,
      use_item: true,
      hold_use: true,
      attack_facing_entity: true,
      select_hotbar_slot: true,
      send_chat: true,
    };

    expect([...ACTION_KINDS].sort()).toEqual(Object.keys(every).sort());
  });

  it("holds nothing only an answer carries, so no answer can be echoed (R5)", () => {
    const answer = mode(1, {
      unlisted_command: true,
      steps: [shown(asked({ type: "send_chat", message: "/warp farm" }))],
    });

    const values = valuesOf(answer);
    const [request] = stepsOf(values).steps;

    expect(Object.keys(values).sort()).toEqual(
      ["atStart", "name", "onRespawn", "repeating", "visibility"].sort(),
    );
    expect(Object.keys(request?.action ?? {}).sort()).toEqual(["message", "type"]);
    expect(Object.keys(request ?? {}).sort()).toEqual(["action", "probability", "schedule"]);
  });

  it("sends only the fields a step's kind takes, whatever else its card still holds", () => {
    const values = newValues();
    values.atStart.push({
      ...newStep(),
      kind: "jump",
      yaw: "1",
      pitch: "2",
      maxYaw: "3",
      maxPitch: "4",
      slot: "5",
      message: "hello",
      every: "6",
      jitter: "7",
    });

    expect(stepsOf(values).steps).toEqual([asked({ type: "jump" })]);
  });

  it("sends the keys 1 to 9 as the API's 0 to 8", () => {
    const values = newValues();
    for (const slot of ["1", "9"]) {
      values.atStart.push({ ...newStep(), kind: "select_hotbar_slot", slot });
    }

    expect(stepsOf(values).steps.map((step) => step.action.slot)).toEqual([0, 8]);
    expect(valuesOf(holding(asked({ type: "select_hotbar_slot", slot: 8 }))).atStart[0]?.slot).toBe(
      "9",
    );
  });

  it("leaves an empty number out for the server, but an empty chance is refused locally (S82)", () => {
    const values = newValues();
    values.repeating.push({ ...newStep(), kind: "look", chance: " " });

    const [step] = stepsOf(values).steps;

    expect(step?.action).toEqual({ type: "look" });
    expect(step?.schedule).toEqual({ type: "every" });
    expect(step?.probability).toBeNaN();
  });

  it("starts a new step as a sure jump, and a new mode empty and private", () => {
    expect(newStep()).toMatchObject({ kind: "jump", chance: "100", cutCommand: null });
    expect(newValues()).toEqual({
      name: "",
      visibility: "private",
      atStart: [],
      repeating: [],
      onRespawn: [],
    });
  });
});

describe("the sections (U68, S84)", () => {
  const first = asked({ type: "jump" }, EVERY);
  const second = asked({ type: "send_chat", message: "hello" }, AT_START);
  const third = asked({ type: "swing_arm" }, EVERY);
  const fourth = asked({ type: "use_item" }, ON_RESPAWN);
  const fifth = asked({ type: "sneak", on: true }, AT_START);

  it("opens an interleaved mode's steps in their sections, each kept in its order", () => {
    const values = valuesOf(holding(first, second, third, fourth, fifth));

    expect(values.atStart.map((step) => step.kind)).toEqual(["send_chat", "sneak"]);
    expect(values.repeating.map((step) => step.kind)).toEqual(["jump", "swing_arm"]);
    expect(values.onRespawn.map((step) => step.kind)).toEqual(["use_item"]);
    expect(stepTotal(values)).toBe(5);
  });

  it("sends the sections in order, each in its shown order", () => {
    const values = valuesOf(holding(first, second, third, fourth, fifth));

    expect(stepsOf(values).steps).toEqual([second, fifth, first, third, fourth]);
  });

  it("maps each path through the order sent, each field to its own", () => {
    const { paths } = stepsOf(valuesOf(holding(first, second, third, fourth, fifth)));

    expect(paths["steps[0].action.message"]).toBe("atStart.0.message");
    expect(paths["steps[1].action.on"]).toBe("atStart.1.on");
    expect(paths["steps[2].schedule.interval_ms"]).toBe("repeating.0.every");
    expect(paths["steps[3].schedule.jitter_ms"]).toBe("repeating.1.jitter");
    expect(paths["steps[4].probability"]).toBe("onRespawn.0.chance");
    expect(paths["steps[4]"]).toBe("onRespawn.0.kind");
    expect(paths["steps[2].action"]).toBe("repeating.0.kind");
    expect(paths["steps[1].action.yaw"]).toBe("atStart.1.yaw");
    expect(paths["steps[1].action.pitch"]).toBe("atStart.1.pitch");
    expect(paths["steps[1].action.max_yaw"]).toBe("atStart.1.maxYaw");
    expect(paths["steps[1].action.max_pitch"]).toBe("atStart.1.maxPitch");
    expect(paths["steps[1].action.slot"]).toBe("atStart.1.slot");
    expect(paths.name).toBe("name");
    expect(paths.visibility).toBe("visibility");
    expect(paths["steps[5]"]).toBeUndefined();
  });

  it.each<[ModeScheduleKind, string | null]>([
    ["at_start", "atStart"],
    ["every", "repeating"],
    ["on_respawn", "onRespawn"],
    ["at_dawn" as ModeScheduleKind, null],
  ])("puts a step at %s in section %s", (type, section) => {
    expect(sectionOf({ type })).toBe(section);
  });
});

describe("the time fields (U72, S82)", () => {
  it.each<[number | undefined, string, string]>([
    [45_000, "45", "seconds"],
    [60_000, "1", "minutes"],
    [90_000, "90", "seconds"],
    [3_600_000, "1", "hours"],
    [5_400_000, "90", "minutes"],
    [86_400_000, "24", "hours"],
    [250, "0.25", "seconds"],
    [1_500, "1.5", "seconds"],
    [0, "0", "seconds"],
    [undefined, "", "seconds"],
  ])("opens %s ms in the largest unit that shows it whole", (ms, text, unit) => {
    expect(timeOf(ms)).toEqual({ text, unit });
  });

  it.each<[string, "seconds" | "minutes" | "hours", number]>([
    ["45", "seconds", 45_000],
    ["1.5", "minutes", 90_000],
    ["2", "hours", 7_200_000],
    ["0.1", "seconds", 100],
    ["0.25", "seconds", 250],
    // Not whole milliseconds: it stays so, for the local refusal, rather than being rounded.
    ["0.0005", "seconds", 0.5],
  ])("sends %s %s as %s ms", (every, everyUnit, ms) => {
    const values = newValues();
    values.repeating.push({ ...newStep(), every, everyUnit, jitter: "0" });

    expect(stepsOf(values).steps[0]?.schedule.interval_ms).toBe(ms);
  });

  it("keeps a loaded wait's meaning when it's sent back", () => {
    const step = asked({ type: "jump" }, { type: "every", interval_ms: 1_234, jitter_ms: 90_000 });

    expect(stepsOf(valuesOf(holding(step))).steps[0]?.schedule).toEqual(step.schedule);
  });
});

describe("what opens in a form (R160, S83)", () => {
  const known = asked({ type: "jump" });

  it("opens an editable mode with every part known", () => {
    expect(isEditable(holding(known))).toBe(true);
    expect(isKnownMode(holding(known))).toBe(true);
  });

  it.each<[string, Partial<ModeResponse>]>([
    ["the server says no", { editable: false }],
    ["an unknown visibility", { visibility: "secret" as ModeVisibility }],
    ["an unknown action", { steps: [shown(asked({ type: "dance" as ModeActionKind }))] }],
    [
      "an unknown schedule",
      {
        steps: [shown(asked({ type: "jump" }, { type: "at_dawn" as ModeScheduleKind } as never))],
      },
    ],
    [
      "a cut command, even if the row says editable",
      { steps: [shown(asked({ type: "send_chat", message: "/warp" }), true)] },
    ],
  ])("never opens one with %s", (_name, changes) => {
    expect(isEditable(mode(1, changes))).toBe(false);
  });

  it("says which steps it can't hold, by the section they'd go in", () => {
    const answer = mode(1, {
      steps: [
        shown(asked({ type: "jump" })),
        shown(asked({ type: "dance" as ModeActionKind }, EVERY)),
        shown(asked({ type: "jump" }, { type: "at_dawn" as ModeScheduleKind } as never)),
      ],
    });

    expect(isKnownMode(answer)).toBe(false);
    expect(unknownSteps(answer)).toEqual(["repeating", null]);
    expect(valuesOf(answer).atStart).toHaveLength(1);
    expect(stepTotal(valuesOf(answer))).toBe(1);
  });
});

describe("a copy (U63, U67, R170)", () => {
  it("starts from the shown mode's steps, with the suggested name, private", () => {
    const shared = mode(1, {
      name: "Warp",
      visibility: "shared",
      steps: [shown(asked({ type: "jump" }, EVERY))],
    });

    const values = copyValues(shared);

    expect(values.name).toBe("Warp copy");
    expect(values.visibility).toBe("private");
    expect(values.repeating).toHaveLength(1);
  });

  it("holds a cut command as it came, marked, and nothing it doesn't hold", () => {
    const shared = mode(1, {
      editable: false,
      steps: [shown(asked({ type: "send_chat", message: "/warp" }), true)],
    });

    const [step] = copyValues(shared).atStart;

    expect(step?.message).toBe("/warp");
    expect(step?.cutCommand).toBe("/warp");
  });

  it("marks no whole command", () => {
    const whole = holding(asked({ type: "send_chat", message: "/warp farm" }));

    expect(copyValues(whole).atStart[0]?.cutCommand).toBeNull();
  });
});
