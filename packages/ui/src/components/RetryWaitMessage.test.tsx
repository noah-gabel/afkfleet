import { act, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useRetryWait } from "../hooks/useRetryWait";
import { fakeTime } from "../test/renderApp";
import { refusal } from "../test/signIn";
import { RetryWaitMessage, TRY_AGAIN } from "./RetryWaitMessage";

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
});
