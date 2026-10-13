import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { SessionPage } from "../../generated/SessionPage";
import type { SessionResponse } from "../../generated/SessionResponse";
import { formatTime } from "../../lib/time";
import { deferred } from "../../test/fixtures";
import { atScreen } from "../../test/signedIn";
import { refusal } from "../../test/signIn";

/** The session numbered `n`, created `n` hours after the first, current or not. */
function session(n: number, current = false): SessionResponse {
  const hour = String(n).padStart(2, "0");
  return {
    id: `01900000-0000-7000-8000-0000000000${String(10 + n)}`,
    created_at: `2026-10-13T${hour}:00:00.000Z`,
    last_used_at: `2026-10-13T${hour}:30:00.000Z`,
    expires_at: `2026-10-20T${hour}:30:00.000Z`,
    current,
  };
}

const CURRENT = session(1, true);

/** The button that logs out the session numbered `n`. */
function logOutButton(n: number): HTMLElement {
  return screen.getByRole("button", {
    name: `Log out the session logged in ${formatTime(session(n).created_at)}`,
  });
}

/** The sessions table. */
async function table(): Promise<HTMLElement> {
  return screen.findByRole("table", { name: "Sessions" });
}

/** The ids `revokeSession` was called with, in order. */
function revoked(app: Awaited<ReturnType<typeof atScreen>>): unknown[] {
  return app.fake.calls.filter((call) => call.method === "revokeSession").map((c) => c.args[0]);
}

describe("the session list", () => {
  it("lists every live session with its times, marking this one", async () => {
    await atScreen("/profile", { sessions: async () => ({ items: [CURRENT, session(2)] }) });

    const rows = within(await table()).getAllByRole("row");
    expect(rows).toHaveLength(3);
    const current = within(rows[1] as HTMLElement);
    expect(current.getByText("This session")).toBeDefined();
    expect(current.getByText(formatTime(CURRENT.created_at))).toBeDefined();
    expect(current.getByText(formatTime(CURRENT.last_used_at))).toBeDefined();
    expect(current.getByText(formatTime(CURRENT.expires_at))).toBeDefined();
    expect(within(rows[2] as HTMLElement).queryByText("This session")).toBeNull();
  });

  it("shows more sessions on request", async () => {
    const app = await atScreen("/profile", {
      sessions: async (before) =>
        before === undefined
          ? { items: [CURRENT, session(2)], next: session(2).id }
          : { items: [session(3)] },
    });
    await table();

    await app.user.click(screen.getByRole("button", { name: "Show more" }));

    expect(await screen.findByText(formatTime(session(3).created_at))).toBeDefined();
    expect(
      app.fake.calls.filter((call) => call.method === "sessions").map((c) => c.args[0]),
    ).toEqual([undefined, session(2).id]);
    expect(screen.queryByRole("button", { name: "Show more" })).toBeNull();
  });

  it("logs out another session at once, then lists them again", async () => {
    let page: SessionPage = { items: [CURRENT, session(2)] };
    const app = await atScreen("/profile", {
      sessions: async () => page,
      revokeSession: async () => {
        page = { items: [CURRENT] };
      },
    });
    await table();

    await app.user.click(logOutButton(2));

    await waitFor(() =>
      expect(within(screen.getByRole("table")).getAllByRole("row")).toHaveLength(2),
    );
    expect(revoked(app)).toEqual([session(2).id]);
  });

  it("shows why another session couldn't be logged out", async () => {
    const app = await atScreen("/profile", {
      sessions: async () => ({ items: [CURRENT, session(2)] }),
      revokeSession: async () => {
        throw refusal(429, "rate_limited", { retry_after_secs: 5 });
      },
    });
    await table();

    await app.user.click(logOutButton(2));

    expect(await screen.findByText(/refused: rate_limited Try again in 5 s\./)).toBeDefined();
  });

  it("logs this session out like the header does", async () => {
    const app = await atScreen("/profile", {
      sessions: async () => ({ items: [CURRENT, session(2)] }),
      logout: async () => ({ server: "ended" }),
    });
    await table();

    await app.user.click(screen.getByRole("button", { name: "Log out this session" }));

    await waitFor(() => expect(app.path()).toBe("/login"));
    expect(revoked(app)).toEqual([]);
  });
});

describe("logging out every other session", () => {
  it("asks first, and Cancel sends nothing", async () => {
    const app = await atScreen("/profile", {
      sessions: async () => ({ items: [CURRENT, session(2)] }),
    });
    await table();

    await app.user.click(screen.getByRole("button", { name: "Log out all other sessions" }));
    expect(
      screen.getByText(/logs out every other session of your account, on every PC/),
    ).toBeDefined();
    await app.user.click(screen.getByRole("button", { name: "Cancel" }));

    expect(revoked(app)).toEqual([]);
    expect(screen.getByRole("button", { name: "Log out all other sessions" })).toBeDefined();
  });

  it("logs out every other session the server lists, on every page, not only the loaded ones", async () => {
    const pages: Record<string, SessionPage> = {
      first: { items: [session(2), CURRENT, session(3)], next: session(3).id },
      [session(3).id]: { items: [session(4), session(5)] },
    };
    const app = await atScreen("/profile", {
      sessions: async (before) => pages[before ?? "first"] ?? { items: [] },
      revokeSession: async () => {},
    });
    await table();
    expect(screen.queryByText(formatTime(session(5).created_at))).toBeNull();

    await app.user.click(screen.getByRole("button", { name: "Log out all other sessions" }));
    await app.user.click(
      within(screen.getByRole("alert")).getByRole("button", { name: "Log out all other sessions" }),
    );

    expect(await screen.findByText("Logged out 4 other sessions.")).toBeDefined();
    expect(revoked(app)).toEqual([session(2).id, session(3).id, session(4).id, session(5).id]);
  });

  it("counts a session already gone as ended and reports one that couldn't be ended", async () => {
    const app = await atScreen("/profile", {
      sessions: async () => ({ items: [CURRENT, session(2), session(3), session(4)] }),
      revokeSession: async (id) => {
        if (id === session(2).id) {
          throw refusal(404, "not_found");
        }
        if (id === session(3).id) {
          throw refusal(503, "busy", { retry_after_secs: 2 });
        }
      },
    });
    await table();

    await app.user.click(screen.getByRole("button", { name: "Log out all other sessions" }));
    await app.user.click(
      within(screen.getByRole("alert")).getByRole("button", { name: "Log out all other sessions" }),
    );

    expect(
      await screen.findByText(
        "Logged out 2 other sessions. 1 couldn't be logged out: refused: busy Try again in 2 s. Try again.",
      ),
    ).toBeDefined();
  });

  it("runs to the end when the screen is left, and reports in a toast", async () => {
    const held = deferred<void>();
    const app = await atScreen("/profile", {
      sessions: async () => ({ items: [CURRENT, session(2), session(3)] }),
      revokeSession: async (id) => {
        if (id === session(2).id) {
          await held.promise;
        }
      },
    });
    await table();
    await app.user.click(screen.getByRole("button", { name: "Log out all other sessions" }));
    await app.user.click(
      within(screen.getByRole("alert")).getByRole("button", { name: "Log out all other sessions" }),
    );
    await waitFor(() => expect(revoked(app)).toEqual([session(2).id]));

    await act(() => app.runtime.router.navigate({ to: "/home" }));
    await act(async () => held.resolve());

    expect(await screen.findByText("Logged out 2 other sessions.")).toBeDefined();
    expect(revoked(app)).toEqual([session(2).id, session(3).id]);
    expect(app.path()).toBe("/home");
  });
});
