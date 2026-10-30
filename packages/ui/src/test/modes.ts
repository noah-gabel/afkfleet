// What the modes' screens' tests script their FakeApiClient with (Plan.md P11.9): a mode's steps
// as answers show them. Placeholders only (CLAUDE.md).
import type { ModeAction } from "../generated/ModeAction";
import type { ModeSchedule } from "../generated/ModeSchedule";
import type { ModeStep } from "../generated/ModeStep";

/** A step of `action`, on `schedule`, as an answer shows it. */
export function shownStep(
  action: Omit<ModeAction, "arguments_hidden"> & { readonly arguments_hidden?: boolean },
  schedule: ModeSchedule = { type: "at_start" },
  probability = 100,
): ModeStep {
  return { action: { arguments_hidden: false, ...action }, schedule, probability };
}

/** A repeating schedule: `seconds` apart, and up to `jitter` more. */
export function every(seconds: number, jitter = 0): ModeSchedule {
  return { type: "every", interval_ms: seconds * 1000, jitter_ms: jitter * 1000 };
}

/** The on-respawn schedule. */
export const ON_RESPAWN: ModeSchedule = { type: "on_respawn" };
