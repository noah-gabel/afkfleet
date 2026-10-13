import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { fakeApiError } from "../api/FakeApiClient";
import { fakeTime } from "../test/renderApp";
import { refusal } from "../test/signIn";
import { retryRequest, useRetryWait } from "./useRetryWait";

const LIMITED = refusal(429, "rate_limited", { retry_after_secs: 3 }).error;

/** The hook, with fake time. */
function waitHook() {
  fakeTime();
  return renderHook(() => useRetryWait());
}

/** Moves fake time on by `ms`. */
function advance(ms: number) {
  act(() => {
    vi.advanceTimersByTime(ms);
  });
}

describe("useRetryWait", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("counts the server's wait down a second at a time, then ends once", () => {
    const { result } = waitHook();

    let started = false;
    act(() => {
      started = result.current.start(LIMITED);
    });
    const first = result.current.waiting;
    advance(999);
    const almost = result.current.waiting?.secondsLeft;
    advance(1);
    const second = result.current.waiting?.secondsLeft;
    advance(2000);

    expect(started).toBe(true);
    expect(first).toEqual({ message: "refused: rate_limited", seconds: 3, secondsLeft: 3 });
    expect(almost).toBe(3);
    expect(second).toBe(2);
    expect(result.current.waiting).toBeNull();
    expect(result.current.ended).toBe(true);
  });

  it("starts no wait for an error without one", () => {
    const { result } = waitHook();

    let started = true;
    act(() => {
      started =
        result.current.start(refusal(403, "forbidden").error) ||
        result.current.start(fakeApiError("unreachable").error);
    });

    expect(started).toBe(false);
    expect(result.current.waiting).toBeNull();
    expect(result.current.ended).toBe(false);
  });

  it("waits for a busy server's wait too", () => {
    expect(retryRequest(refusal(503, "busy", { retry_after_secs: 2 }).error)).toEqual({
      message: "refused: busy",
      seconds: 2,
    });
  });

  it("replaces a running wait with a new one", () => {
    const { result } = waitHook();
    act(() => {
      result.current.start(LIMITED);
    });
    advance(2000);

    act(() => {
      result.current.start(refusal(429, "rate_limited", { retry_after_secs: 5 }).error);
    });
    advance(4000);

    expect(result.current.waiting?.secondsLeft).toBe(1);
  });

  it("forgets an ended wait's notice on reset, as a new submit does", () => {
    const { result } = waitHook();
    act(() => {
      result.current.start(LIMITED);
    });
    advance(3000);

    act(() => {
      result.current.reset();
    });

    expect(result.current.ended).toBe(false);
    expect(result.current.waiting).toBeNull();
  });

  it("starts a wait a notice carries", () => {
    const { result } = waitHook();

    act(() => {
      result.current.startFor({ message: "refused: rate_limited", seconds: 2 });
    });

    expect(result.current.waiting?.secondsLeft).toBe(2);
  });
});
