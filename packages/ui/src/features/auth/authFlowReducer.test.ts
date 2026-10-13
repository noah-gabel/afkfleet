import { describe, expect, it } from "vitest";

import type { MfaEnrollResponse } from "../../generated/MfaEnrollResponse";
import {
  authFlowReducer,
  type FlowState,
  initialFlow,
  REGISTERED,
  STEP_ENDED,
  wrongCode,
} from "./authFlowReducer";

const NOW = 1_800_000_000_000;
const ENROLLMENT: MfaEnrollResponse = {
  otpauth_uri: "otpauth://totp/afkfleet:afkbot1?secret=ABCD",
  qr_svg: "data:image/svg+xml;base64,PHN2Zy8+",
  secret: "ABCDEFGHIJKLMNOPQRST",
};

function setup(): FlowState {
  return authFlowReducer(initialFlow("login"), {
    type: "setupRequired",
    from: 0,
    username: "afkbot1",
    password: "placeholder password",
    expiresIn: 600,
    now: NOW,
  });
}

function code(): FlowState {
  return authFlowReducer(initialFlow("login"), {
    type: "codeRequired",
    from: 0,
    username: "afkbot1",
    expiresIn: 300,
    now: NOW,
  });
}

describe("authFlowReducer", () => {
  it("starts at the form of its mode, holding nothing", () => {
    expect(initialFlow("login")).toEqual({
      step: { kind: "password" },
      stepId: 0,
      username: "",
      password: null,
    });
    expect(initialFlow("register").step).toEqual({ kind: "register" });
  });

  it("starts the login form with what a failed registration hands over", () => {
    const state = initialFlow("login", {
      username: "newbot",
      notice: { text: REGISTERED, reason: "fake unreachable" },
    });

    expect(state.username).toBe("newbot");
    expect(state.step).toEqual({
      kind: "password",
      notice: { text: REGISTERED, reason: "fake unreachable" },
    });
  });

  it("goes to the code step in a new step, without keeping the password", () => {
    const state = code();

    expect(state.step).toEqual({
      kind: "code",
      deadline: NOW + 300_000,
      expiresIn: 300,
      triesLeft: 3,
      next: false,
    });
    expect(state.stepId).toBe(1);
    expect(state.username).toBe("afkbot1");
    expect(state.password).toBeNull();
  });

  it("keeps the password through the setup and the recovery codes", () => {
    const confirmed = authFlowReducer(setup(), { type: "confirmed", from: 1, codes: ["a-b"] });

    expect(setup().step).toEqual({
      kind: "setup",
      deadline: NOW + 600_000,
      expiresIn: 600,
      loading: true,
    });
    expect(setup().password).toBe("placeholder password");
    expect(confirmed.step).toEqual({ kind: "recovery", codes: ["a-b"] });
    expect(confirmed.password).toBe("placeholder password");
    expect(confirmed.stepId).toBe(2);
  });

  it("shows a secret in its step, and hides it while a new one loads", () => {
    const shown = authFlowReducer(setup(), { type: "enrolled", from: 1, enrollment: ENROLLMENT });
    const reloading = authFlowReducer(shown, { type: "enrolling", from: 1 });
    const failed = authFlowReducer(reloading, { type: "enrollFailed", from: 1, message: "x" });

    expect(shown.step).toMatchObject({ kind: "setup", loading: false, enrollment: ENROLLMENT });
    expect(shown.stepId).toBe(1);
    expect(reloading.step).toMatchObject({ kind: "setup", loading: true });
    expect(reloading.step).not.toHaveProperty("enrollment");
    expect(failed.step).toMatchObject({ kind: "setup", loading: false, enrollError: "x" });
  });

  it("drops the password at the code step after the setup, which waits for the next code", () => {
    const recovery = authFlowReducer(setup(), { type: "confirmed", from: 1, codes: ["a-b"] });

    const next = authFlowReducer(recovery, {
      type: "codeRequired",
      from: 2,
      username: "afkbot1",
      expiresIn: 300,
      now: NOW + 60_000,
      next: true,
    });

    expect(next.password).toBeNull();
    expect(next.step).toMatchObject({ kind: "code", next: true, triesLeft: 3 });
  });

  it("counts wrong codes in their step and ends it at the third", () => {
    const first = authFlowReducer(code(), { type: "wrongCode", from: 1 });
    const second = authFlowReducer(first, { type: "wrongCode", from: 1 });
    const third = authFlowReducer(second, { type: "wrongCode", from: 1 });

    expect(first.step).toMatchObject({ kind: "code", triesLeft: 2, message: wrongCode(2) });
    expect(first.stepId).toBe(1);
    expect(second.step).toMatchObject({ kind: "code", triesLeft: 1, message: wrongCode(1) });
    expect(third.step).toEqual({ kind: "password", notice: { text: STEP_ENDED } });
    expect(third.username).toBe("afkbot1");
    expect(third.stepId).toBe(2);
  });

  it("drops an answer from a step that's no longer current", () => {
    const ended = authFlowReducer(code(), { type: "stepEnded", from: 1 });
    const again = authFlowReducer(ended, {
      type: "codeRequired",
      from: 2,
      username: "afkbot1",
      expiresIn: 300,
      now: NOW,
    });

    expect(authFlowReducer(again, { type: "wrongCode", from: 1 })).toBe(again);
    expect(authFlowReducer(ended, { type: "confirmed", from: 1, codes: ["a-b"] })).toBe(ended);
    expect(
      authFlowReducer(ended, {
        type: "setupRequired",
        from: 0,
        username: "x",
        password: "y",
        expiresIn: 600,
        now: NOW,
      }),
    ).toBe(ended);
  });

  it.each([
    ["the setup step", setup()],
    ["the recovery codes", authFlowReducer(setup(), { type: "confirmed", from: 1, codes: ["x"] })],
  ])("drops the password when %s ends", (_, state) => {
    const ended = authFlowReducer(state, { type: "stepEnded", from: state.stepId });

    expect(ended.step).toEqual({ kind: "password", notice: { text: STEP_ENDED } });
    expect(ended.password).toBeNull();
    expect(ended.username).toBe("afkbot1");
    expect(ended.stepId).toBe(state.stepId + 1);
  });

  it("drops the password when the flow goes back to the password step", () => {
    const back = authFlowReducer(setup(), {
      type: "backToPassword",
      from: 1,
      notice: { text: REGISTERED, reason: "fake rate_limited" },
    });

    expect(back).toEqual({
      step: { kind: "password", notice: { text: REGISTERED, reason: "fake rate_limited" } },
      stepId: 2,
      username: "afkbot1",
      password: null,
    });
  });

  it("says who is still signed in, holding no password", () => {
    const still = authFlowReducer(setup(), { type: "stillSignedIn", from: 1, username: "other" });

    expect(still.step).toEqual({ kind: "stillSignedIn", username: "other" });
    expect(still.password).toBeNull();
  });

  it("ignores a wrong code and a secret outside their steps", () => {
    expect(authFlowReducer(setup(), { type: "wrongCode", from: 1 })).toEqual(setup());
    expect(authFlowReducer(code(), { type: "enrolled", from: 1, enrollment: ENROLLMENT })).toEqual(
      code(),
    );
  });
});
