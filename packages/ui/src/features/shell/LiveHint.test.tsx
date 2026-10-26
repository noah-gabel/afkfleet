import { screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ApiError } from "../../api/ApiError";
import { fakeWatch } from "../../api/FakeApiClient";
import { disconnected, refetch } from "../../test/live";
import { fakeTime } from "../../test/renderApp";
import { signedIn } from "../../test/signedIn";

afterEach(() => {
  vi.useRealTimers();
});

/** The Header, where the hint shows. */
function header() {
  return within(screen.getByRole("banner"));
}

/** The hint's screen reader text: the Header's one status region. */
function spoken(): string {
  return header().getByRole("status").textContent ?? "";
}

/** The Header's text without the screen reader's. */
function shown(): string {
  const banner = screen.getByRole("banner").cloneNode(true) as HTMLElement;
  for (const status of banner.querySelectorAll("[role=status]")) {
    status.remove();
  }
  return banner.textContent ?? "";
}

/** The App signed in, its live events on a fake watch, fake time running. */
async function live() {
  const time = fakeTime();
  const watch = fakeWatch();
  const app = await signedIn({ watchEvents: watch.handler }, { advanceTimers: time.advanceTimers });
  return { app, watch, time };
}

const TOO_MANY =
  "Live updates are already open in too many places. Close the app somewhere else, or try again in a minute.";

describe("the live events' hint", () => {
  it("counts a reconnect's wait down, and tells a screen reader once", async () => {
    const { watch, time } = await live();

    watch.latest().push(disconnected(15));
    await time.advance(0);
    const first = shown();
    const firstSpoken = spoken();
    await time.advance(5_000);
    const later = shown();
    const laterSpoken = spoken();
    await time.advance(10_000);

    expect(first).toContain("Live updates paused. Reconnecting in 15 s.");
    expect(firstSpoken).toBe("Live updates paused. Reconnecting in 15 seconds.");
    expect(later).toContain("Live updates paused. Reconnecting in 10 s.");
    expect(laterSpoken).toBe(firstSpoken);
    expect(shown()).not.toContain("Live updates");
    expect(spoken()).toBe("");
  });

  it("says nothing for the routine reconnect, and goes once the socket is live again", async () => {
    const { watch, time } = await live();

    watch.latest().push(disconnected(0));
    await time.advance(0);
    const routine = shown();
    watch.latest().push(disconnected(4));
    await time.advance(1_000);
    watch.latest().push(refetch("ready"));
    await time.advance(0);

    expect(routine).not.toContain("Live updates");
    expect(shown()).not.toContain("Live updates");
    expect(spoken()).toBe("");
  });

  it("says the app is open in too many places after too_many_connections", async () => {
    const { watch, time } = await live();

    watch.latest().push({
      update: "disconnected",
      disconnect: {
        why: "not_opened",
        error: {
          code: "api",
          message: "The server refused the request.",
          api: {
            status: 429,
            code: "too_many_connections",
            message: TOO_MANY,
            request_id: "r-1",
            retry_after_secs: 45,
          },
        },
      },
      retry_in_secs: 45,
    });
    await time.advance(0);

    expect(shown()).toContain(TOO_MANY);
    expect(shown()).not.toContain("Reconnecting");
    expect(spoken()).toBe(TOO_MANY);
  });

  it.each([
    [
      "certificate",
      { code: "certificate", message: "The server's certificate isn't trusted." },
      "Live updates stopped: The server's certificate isn't trusted. Screens update when you open them.",
    ],
    [
      "a refusal without a period",
      {
        code: "api",
        message: "The server refused the request.",
        api: { status: 418, code: "from_a_newer_server", message: "Refused", request_id: "r-1" },
      },
      "Live updates stopped: Refused (request r-1). Screens update when you open them.",
    ],
  ] as const)("says the watch stopped after %s, with why", async (_, error, text) => {
    const { watch, time } = await live();

    watch.latest().fail(new ApiError(error as ConstructorParameters<typeof ApiError>[0]));
    await time.advance(0);

    expect(shown()).toContain(text);
    expect(spoken()).toBe(text);
  });

  it("says nothing when the watch ends with the session", async () => {
    const { watch, time } = await live();

    watch.latest().fail(new ApiError({ code: "no_session", message: "You're not logged in." }));
    await time.advance(0);

    expect(screen.queryByText(/Live updates/)).toBeNull();
  });
});
