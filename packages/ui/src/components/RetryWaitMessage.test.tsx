import { act, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useRetryWait } from "../hooks/useRetryWait";
import { fakeTime } from "../test/renderApp";
import { refusal } from "../test/signIn";
import { RetryWaitMessage, spokenWait, TRY_AGAIN, visibleWait } from "./RetryWaitMessage";

/** A form's error area with the wait of a 429 that asks for `seconds`. */
function Harness({ seconds }: { seconds: number }) {
  const wait = useRetryWait();
  return (
    <>
      <button
        type="button"
        onClick={() =>
          wait.start(refusal(429, "rate_limited", { retry_after_secs: seconds }).error)
        }
      >
        Refuse
      </button>
      <RetryWaitMessage wait={wait} />
    </>
  );
}

/** What a screen reader reads of `element`: its text without what's hidden from it. */
function spoken(element: HTMLElement): string {
  const copy = element.cloneNode(true) as HTMLElement;
  for (const hidden of copy.querySelectorAll('[aria-hidden="true"]')) {
    hidden.remove();
  }
  return copy.textContent ?? "";
}

function advance(ms: number) {
  act(() => {
    vi.advanceTimersByTime(ms);
  });
}

describe("RetryWaitMessage", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("shows nothing without a wait", () => {
    render(<Harness seconds={3} />);

    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("counts down on screen, while a screen reader hears the whole wait once", () => {
    fakeTime();
    render(<Harness seconds={30} />);
    act(() => screen.getByRole("button", { name: "Refuse" }).click());
    const alert = screen.getByRole("alert");
    const heard = spoken(alert);

    advance(1000);

    expect(heard).toBe("refused: rate_limited Try again in 30 seconds.");
    expect(spoken(alert)).toBe(heard);
    expect(screen.getByText("Try again in 29 s.")).toBeDefined();
  });

  it("says once, as a status, that the user can try again", () => {
    fakeTime();
    render(<Harness seconds={2} />);
    act(() => screen.getByRole("button", { name: "Refuse" }).click());

    advance(2000);

    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByRole("status").textContent).toBe(TRY_AGAIN);
  });

  it.each([
    [1, "Try again in 1 s."],
    [60, "Try again in 60 s."],
    [61, "Try again in 2 min."],
    [120, "Try again in 2 min."],
    [3600, "Try again in 60 min."],
    [3601, "Try again in 1 h 1 min."],
    [3661, "Try again in 1 h 2 min."],
    [7200, "Try again in 2 h."],
    [86_400, "Try again in 24 h."],
  ])("shows a wait of %i s as %s", (seconds, text) => {
    expect(visibleWait(seconds)).toBe(text);
  });

  it.each([
    [1, "Try again in 1 second."],
    [60, "Try again in 60 seconds."],
    [61, "Try again in 2 minutes."],
    [3600, "Try again in 60 minutes."],
    [3601, "Try again in 1 hour 1 minute."],
    [7260, "Try again in 2 hours 1 minute."],
    [7200, "Try again in 2 hours."],
  ])("says a wait of %i s as %s", (seconds, text) => {
    expect(spokenWait(seconds)).toBe(text);
  });

  it("switches from minutes to seconds as a long wait runs down", () => {
    fakeTime();
    render(<Harness seconds={90} />);
    act(() => screen.getByRole("button", { name: "Refuse" }).click());

    expect(screen.getByText("Try again in 2 min.")).toBeDefined();
    advance(30_000);
    expect(screen.getByText("Try again in 60 s.")).toBeDefined();
    expect(spoken(screen.getByRole("alert"))).toContain("Try again in 2 minutes.");
  });
});
