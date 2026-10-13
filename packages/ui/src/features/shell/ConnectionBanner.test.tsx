import { act, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { fakeApiError } from "../../api/FakeApiClient";
import type { ApiErrorDetails } from "../../generated/ApiErrorDetails";
import type { MeResponse } from "../../generated/MeResponse";
import { deferred, me, PROFILE } from "../../test/fixtures";
import { fakeTime, renderApp } from "../../test/renderApp";
import { CERTIFICATE_HELP } from "../server/certificateHelp";

const DOWN = "Can't reach the server. Trying again every 10 s.";

function refusal(code: string, status: number, retry?: number): ApiErrorDetails {
  return {
    status,
    code: code as ApiErrorDetails["code"],
    message: "The server refused.",
    request_id: "01900000-0000-7000-8000-000000000009",
    ...(retry === undefined ? {} : { retry_after_secs: retry }),
  };
}

/** The App signed in on Home, with `checkServer` answered by `check`. */
async function signedIn(check: () => Promise<void>) {
  const time = fakeTime();
  let next: (() => Promise<MeResponse>) | null = null;
  const app = renderApp(
    {
      serverProfile: async () => PROFILE,
      me: () => (next === null ? Promise.resolve(me()) : next()),
      checkServer: check,
    },
    { advanceTimers: time.advanceTimers },
  );
  await screen.findByText(/Signed in as afkbot1/);
  /** Makes the next `me` calls answer `answer`. */
  const meAnswers = (answer: () => Promise<MeResponse>) => {
    next = answer;
  };
  /** A call fails with `code`, which puts the banner up. */
  const fail = async (code: "unreachable" | "timed_out" | "certificate") => {
    meAnswers(async () => {
      throw fakeApiError(code);
    });
    await act(() => app.runtime.queryClient.refetchQueries({ queryKey: ["me"] }));
  };
  const checks = () => app.fake.calls.filter((call) => call.method === "checkServer").length;
  return { app, time, fail, checks, meAnswers };
}

describe("the connection banner", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it.each(["unreachable", "timed_out"] as const)("comes up on %s", async (code) => {
    const { fail } = await signedIn(async () => {});

    await fail(code);

    expect(await screen.findByText(DOWN)).toBeDefined();
  });

  it("checks every 10 s, never two at once", async () => {
    const held = deferred<void>();
    let calls = 0;
    const { fail, time, checks } = await signedIn(() => {
      calls += 1;
      return calls === 1 ? held.promise : Promise.reject(fakeApiError("unreachable"));
    });
    await fail("unreachable");

    await time.advance(9_000);
    const before = checks();
    await time.advance(1_500);
    const first = checks();
    await time.advance(60_000);
    const whileHeld = checks();
    await act(async () => {
      held.reject(fakeApiError("unreachable"));
    });
    await time.advance(10_500);

    expect(before).toBe(0);
    expect(first).toBe(1);
    expect(whileHeld).toBe(1);
    expect(checks()).toBe(2);
  });

  it("checks at once on Retry now", async () => {
    const { app, fail, checks } = await signedIn(async () => {
      throw fakeApiError("unreachable");
    });
    await fail("unreachable");

    await app.user.click(await screen.findByRole("button", { name: "Retry now" }));

    await waitFor(() => expect(checks()).toBe(1));
  });

  it("waits as long as a busy server asks", async () => {
    let calls = 0;
    const { fail, time, checks } = await signedIn(async () => {
      calls += 1;
      throw fakeApiError("api", { api: refusal("busy", 503, 30) });
    });
    await fail("unreachable");
    await time.advance(10_500);

    expect(await screen.findByText("The server is busy. Trying again in 30 s.")).toBeDefined();
    await time.advance(25_000);
    expect(checks()).toBe(1);
    await time.advance(6_000);
    expect(calls).toBe(2);
  });

  it("waits at least 10 s after a rate-limited check", async () => {
    const { fail, time, checks } = await signedIn(async () => {
      throw fakeApiError("api", { api: refusal("rate_limited", 429, 2) });
    });
    await fail("unreachable");
    await time.advance(10_500);

    expect(await screen.findByText("The server is busy. Trying again in 10 s.")).toBeDefined();
    await time.advance(5_000);
    expect(checks()).toBe(1);
    await time.advance(6_000);
    expect(checks()).toBe(2);
  });

  it("keeps trying every 10 s after another failure, and says what it was", async () => {
    const { fail, time, checks } = await signedIn(async () => {
      throw fakeApiError("unexpected_response");
    });
    await fail("unreachable");
    await time.advance(10_500);

    expect(await screen.findByText("fake unexpected_response")).toBeDefined();
    expect(screen.getByText(DOWN)).toBeDefined();
    await time.advance(10_500);
    expect(checks()).toBe(2);
  });

  it("goes away and refetches what failed once a check succeeds", async () => {
    const { app, fail, meAnswers } = await signedIn(async () => {});
    await fail("unreachable");
    const before = app.fake.calls.filter((call) => call.method === "me").length;
    meAnswers(async () => me());

    await app.user.click(await screen.findByRole("button", { name: "Retry now" }));

    await waitFor(() => expect(screen.queryByText(DOWN)).toBeNull());
    await waitFor(() =>
      expect(app.fake.calls.filter((call) => call.method === "me").length).toBe(before + 1),
    );
    expect(app.runtime.queryClient.getQueryState(["me"])?.status).toBe("success");
  });

  it("goes away after any call that succeeds, and refetches what failed", async () => {
    const { app, fail, meAnswers } = await signedIn(async () => {
      throw fakeApiError("unreachable");
    });
    Object.assign(app.fake, { sessions: async () => ({ items: [] }) });
    await fail("unreachable");
    meAnswers(async () => me());

    await act(() => app.runtime.apiClient.sessions());

    await waitFor(() => expect(screen.queryByText(DOWN)).toBeNull());
    await waitFor(() =>
      expect(app.runtime.queryClient.getQueryState(["me"])?.status).toBe("success"),
    );
  });

  it("stays for a success that started before it came up", async () => {
    const held = deferred<{ items: never[] }>();
    const { app, fail } = await signedIn(async () => {
      throw fakeApiError("unreachable");
    });
    Object.assign(app.fake, { sessions: () => held.promise });

    const early = app.runtime.apiClient.sessions();
    await fail("unreachable");
    held.resolve({ items: [] });
    await act(() => early);

    expect(screen.getByText(DOWN)).toBeDefined();
  });

  it("explains a refused certificate, without checking on a timer", async () => {
    const { app, fail, time, checks } = await signedIn(async () => {
      throw fakeApiError("certificate");
    });
    await fail("certificate");

    expect(await screen.findByText(CERTIFICATE_HELP)).toBeDefined();
    await time.advance(60_000);
    expect(checks()).toBe(0);
    await app.user.click(screen.getByRole("button", { name: "Retry now" }));
    await waitFor(() => expect(checks()).toBe(1));
    await app.user.click(screen.getByRole("button", { name: "Server settings" }));
    await waitFor(() => expect(app.path()).toBe("/server"));
  });
});
