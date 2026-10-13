import { describe, expect, it } from "vitest";

import { STEP_UP_WINDOW_SECS } from "../../api/limits";
import { ENROLLMENT } from "../../test/signIn";
import {
  INITIAL_PROFILE_FLOW,
  type ProfileFlowState,
  profileFlowReducer,
  STEP_ENDED,
} from "./profileFlowReducer";

const SENT = 1_800_000_000_000;

/** The flow after `steps` from its start. */
function through(...steps: Parameters<typeof profileFlowReducer>[1][]): ProfileFlowState {
  return steps.reduce(profileFlowReducer, INITIAL_PROFILE_FLOW);
}

describe("profileFlowReducer", () => {
  it("starts at the password", () => {
    expect(INITIAL_PROFILE_FLOW).toEqual({ step: { kind: "confirm" }, stepId: 0 });
  });

  it("goes from the step-up to the new password, ending with the step-up's window", () => {
    const state = through({ type: "steppedUp", from: 0, now: SENT });

    expect(state).toEqual({
      stepId: 1,
      step: {
        kind: "newPassword",
        deadline: SENT + STEP_UP_WINDOW_SECS * 1000,
        expiresIn: STEP_UP_WINDOW_SECS,
      },
    });
  });

  it("goes from the reset to the setup, ending with the setup token, its secret loading", () => {
    const state = through({ type: "resetStarted", from: 0, expiresIn: 600, now: SENT });

    expect(state).toEqual({
      stepId: 1,
      step: { kind: "setup", deadline: SENT + 600_000, expiresIn: 600, loading: true },
    });
  });

  it("shows the setup's secret, then the new recovery codes", () => {
    const enrolled = through(
      { type: "resetStarted", from: 0, expiresIn: 600, now: SENT },
      { type: "enrolled", from: 1, enrollment: ENROLLMENT },
    );
    const confirmed = profileFlowReducer(enrolled, {
      type: "confirmed",
      from: 1,
      codes: ["a", "b"],
    });

    expect(enrolled.step).toMatchObject({ kind: "setup", loading: false, enrollment: ENROLLMENT });
    expect(enrolled.stepId).toBe(1);
    expect(confirmed).toEqual({ stepId: 2, step: { kind: "recovery", codes: ["a", "b"] } });
  });

  it("goes back to the password when a step ends, with the notice", () => {
    const state = through(
      { type: "steppedUp", from: 0, now: SENT },
      { type: "stepEnded", from: 1 },
    );

    expect(state).toEqual({ stepId: 2, step: { kind: "confirm", notice: STEP_ENDED } });
  });

  it("drops an action of a step that is no longer current", () => {
    const ended = through(
      { type: "resetStarted", from: 0, expiresIn: 600, now: SENT },
      { type: "stepEnded", from: 1 },
    );

    const late = [
      profileFlowReducer(ended, { type: "enrolled", from: 1, enrollment: ENROLLMENT }),
      profileFlowReducer(ended, { type: "confirmed", from: 1, codes: ["a"] }),
      profileFlowReducer(ended, { type: "steppedUp", from: 0, now: SENT }),
    ];

    for (const state of late) {
      expect(state).toBe(ended);
    }
  });

  it("ignores an action that doesn't fit the current step", () => {
    const confirm = INITIAL_PROFILE_FLOW;
    const newPassword = through({ type: "steppedUp", from: 0, now: SENT });

    expect(profileFlowReducer(confirm, { type: "enrolled", from: 0, enrollment: ENROLLMENT })).toBe(
      confirm,
    );
    expect(profileFlowReducer(confirm, { type: "confirmed", from: 0, codes: ["a"] })).toBe(confirm);
    expect(
      profileFlowReducer(newPassword, { type: "resetStarted", from: 1, expiresIn: 600, now: SENT }),
    ).toBe(newPassword);
  });
});
