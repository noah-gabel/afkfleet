import { describe, expect, it } from "vitest";

import {
  authFlowReducer,
  type FlowState,
  initialFlow,
  REGISTERED,
  STEP_ENDED,
  wrongCode,
} from "./authFlowReducer";

const NOW = 1_800_000_000_000;

function setup(): FlowState {
  return authFlowReducer(initialFlow("login"), {
    type: "setupRequired",
    username: "afkbot1",
    password: "placeholder password",
    expiresIn: 600,
    now: NOW,
  });
}

describe("authFlowReducer", () => {
  it("starts at the form of its mode, holding nothing", () => {
    expect(initialFlow("login")).toEqual({
      step: { kind: "password" },
      username: "",
      password: null,
      round: 0,
    });
    expect(initialFlow("register").step).toEqual({ kind: "register" });
  });

  it("goes to the code step without keeping the password", () => {
    const state = authFlowReducer(initialFlow("login"), {
      type: "codeRequired",
      username: "afkbot1",
      expiresIn: 300,
      now: NOW,
    });

    expect(state.step).toEqual({
      kind: "code",
      deadline: NOW + 300_000,
      expiresIn: 300,
      triesLeft: 3,
      next: false,
    });
    expect(state.username).toBe("afkbot1");
    expect(state.password).toBeNull();
  });

  it("keeps the password through the setup and the recovery codes", () => {
    const confirmed = authFlowReducer(setup(), { type: "confirmed", codes: ["abcd-efgh"] });

    expect(setup().step).toEqual({ kind: "setup", deadline: NOW + 600_000, expiresIn: 600 });
    expect(setup().password).toBe("placeholder password");
    expect(confirmed.step).toEqual({ kind: "recovery", codes: ["abcd-efgh"] });
    expect(confirmed.password).toBe("placeholder password");
  });

  it("drops the password at the code step after the setup, which waits for the next code", () => {
    const recovery = authFlowReducer(setup(), { type: "confirmed", codes: ["abcd-efgh"] });

    const next = authFlowReducer(recovery, {
      type: "codeRequired",
      username: "afkbot1",
      expiresIn: 300,
      now: NOW + 60_000,
      next: true,
    });

    expect(next.password).toBeNull();
    expect(next.step).toMatchObject({ kind: "code", next: true, triesLeft: 3 });
  });

  it("counts wrong codes and ends the step at the third", () => {
    let state = authFlowReducer(initialFlow("login"), {
      type: "codeRequired",
      username: "afkbot1",
      expiresIn: 300,
      now: NOW,
    });

    state = authFlowReducer(state, { type: "wrongCode" });
    const second = authFlowReducer(state, { type: "wrongCode" });
    const third = authFlowReducer(second, { type: "wrongCode" });

    expect(state.step).toMatchObject({ kind: "code", triesLeft: 2, message: wrongCode(2) });
    expect(second.step).toMatchObject({ kind: "code", triesLeft: 1, message: wrongCode(1) });
    expect(third.step).toEqual({ kind: "password", notice: { text: STEP_ENDED } });
    expect(third.username).toBe("afkbot1");
  });

  it.each([
    ["the setup step", setup()],
    ["the recovery codes", authFlowReducer(setup(), { type: "confirmed", codes: ["x"] })],
  ])("drops the password when %s ends", (_, state) => {
    const ended = authFlowReducer(state, { type: "stepEnded" });

    expect(ended.step).toEqual({ kind: "password", notice: { text: STEP_ENDED } });
    expect(ended.password).toBeNull();
    expect(ended.username).toBe("afkbot1");
    expect(ended.round).toBe(state.round + 1);
  });

  it("drops the password when the flow goes back to the password step", () => {
    const back = authFlowReducer(setup(), {
      type: "backToPassword",
      notice: { text: REGISTERED, reason: "fake rate_limited" },
    });

    expect(back).toEqual({
      step: { kind: "password", notice: { text: REGISTERED, reason: "fake rate_limited" } },
      username: "afkbot1",
      password: null,
      round: 1,
    });
  });

  it("fills in another username when it goes back with one", () => {
    const back = authFlowReducer(initialFlow("register"), {
      type: "backToPassword",
      username: "newbot",
    });

    expect(back.username).toBe("newbot");
    expect(back.step).toEqual({ kind: "password" });
  });

  it("says who is still signed in, holding no password", () => {
    const still = authFlowReducer(setup(), { type: "stillSignedIn", username: "otherbot" });

    expect(still.step).toEqual({ kind: "stillSignedIn", username: "otherbot" });
    expect(still.password).toBeNull();
  });

  it("ignores a wrong code outside the code step", () => {
    expect(authFlowReducer(setup(), { type: "wrongCode" })).toEqual(setup());
  });
});
