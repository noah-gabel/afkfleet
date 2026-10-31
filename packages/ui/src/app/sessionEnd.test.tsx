import type { QueryClient } from "@tanstack/react-query";
import { act, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { fakeApiError } from "../api/FakeApiClient";
import { AGENTS_KEY, AUDIT_KEY, USERS_KEY } from "../api/queries";
import type { AuditPage } from "../generated/AuditPage";
import type { LogoutResponse } from "../generated/LogoutResponse";
import type { SessionPage } from "../generated/SessionPage";
import { deferred, me, PROFILE } from "../test/fixtures";
import { renderApp } from "../test/renderApp";
import { atScreen, onHome } from "../test/signedIn";
import { SESSION_ENDED } from "./leave";

const NOT_FOUND = "This page doesn't exist or isn't available to you.";

const NO_SESSION = async (): Promise<never> => {
  throw fakeApiError("no_session");
};

/** The App, signed in on Home. */
async function signedIn(handlers: Parameters<typeof renderApp>[0] = {}) {
  const app = renderApp({ serverProfile: async () => PROFILE, me: async () => me(), ...handlers });
  await onHome("afkbot1");
  return app;
}

describe("a session that ends during use", () => {
  it("goes to login, empties the cache and says so, after a direct call", async () => {
    const app = await signedIn({ sessions: NO_SESSION });

    await act(() => app.runtime.apiClient.sessions().catch(() => {}));

    await waitFor(() => expect(app.path()).toBe("/login"));
    expect(await screen.findByText(SESSION_ENDED)).toBeDefined();
    expect(app.runtime.queryClient.getQueryData(["me"])).toBeUndefined();
    expect(app.runtime.stores.session.getState().phase).toBe("signed_out");
  });

  it("does the same after a query", async () => {
    let ended = false;
    const app = await signedIn({
      me: async () => {
        if (ended) {
          throw fakeApiError("no_session");
        }
        return me();
      },
    });

    ended = true;
    await act(() => app.runtime.queryClient.invalidateQueries({ queryKey: ["me"] }));

    await waitFor(() => expect(app.path()).toBe("/login"));
    expect(await screen.findByText(SESSION_ENDED)).toBeDefined();
  });

  it("does the same after a mutation", async () => {
    const app = await signedIn({ revokeSession: NO_SESSION });
    const mutation = app.runtime.queryClient.getMutationCache().build(app.runtime.queryClient, {
      mutationFn: () => app.runtime.apiClient.revokeSession("01900000-0000-7000-8000-000000000002"),
    });

    await act(() => mutation.execute(undefined).catch(() => {}));

    await waitFor(() => expect(app.path()).toBe("/login"));
    expect(await screen.findByText(SESSION_ENDED)).toBeDefined();
  });

  it("navigates and says so once when several calls fail at once", async () => {
    const app = await signedIn({ sessions: NO_SESSION, invites: NO_SESSION });
    const navigate = vi.spyOn(app.runtime.router, "navigate");

    await act(() =>
      Promise.all([
        app.runtime.apiClient.sessions().catch(() => {}),
        app.runtime.apiClient.invites().catch(() => {}),
        app.runtime.apiClient.sessions().catch(() => {}),
      ]),
    );

    await waitFor(() => expect(app.path()).toBe("/login"));
    expect(await screen.findAllByText(SESSION_ENDED)).toHaveLength(1);
    expect(navigate).toHaveBeenCalledTimes(1);
  });

  it("leaves the screen before emptying the cache, so nothing refetches", async () => {
    const app = await signedIn({ sessions: NO_SESSION });

    await act(() => app.runtime.apiClient.sessions().catch(() => {}));
    await screen.findByText(SESSION_ENDED);

    expect(app.fake.calls.filter((call) => call.method === "me")).toHaveLength(1);
  });

  it("can't be ended by an answer from before a re-login", async () => {
    const held = deferred<SessionPage>();
    const app = await signedIn({ sessions: () => held.promise });

    const call = app.runtime.apiClient.sessions().catch(() => {});
    act(() => {
      app.runtime.stores.session.getState().signOut();
      app.runtime.stores.session.getState().signIn();
    });
    held.reject(fakeApiError("no_session"));
    await act(() => call);

    expect(app.path()).toBe("/home");
    expect(screen.queryByText(SESSION_ENDED)).toBeNull();
  });

  it("is quiet during and after a deliberate logout", async () => {
    const logout = deferred<LogoutResponse>();
    const app = await signedIn({ sessions: NO_SESSION, logout: () => logout.promise });

    await app.user.click(screen.getByRole("button", { name: "Log out" }));
    await act(() => app.runtime.apiClient.sessions().catch(() => {}));
    logout.resolve({ server: "ended" });
    await waitFor(() => expect(app.path()).toBe("/login"));
    await act(() => app.runtime.apiClient.sessions().catch(() => {}));

    expect(screen.queryByText(SESSION_ENDED)).toBeNull();
  });

  it("ends again after the next sign-in", async () => {
    const app = await signedIn({ sessions: NO_SESSION });
    const navigate = vi.spyOn(app.runtime.router, "navigate");
    await act(() => app.runtime.apiClient.sessions().catch(() => {}));
    await waitFor(() => expect(app.path()).toBe("/login"));

    act(() => app.runtime.stores.session.getState().signIn());
    await act(() => app.runtime.router.navigate({ to: "/home" }));
    await act(() => app.runtime.apiClient.sessions().catch(() => {}));

    await waitFor(() => expect(app.path()).toBe("/login"));
    expect(navigate.mock.calls.filter(([options]) => options.to === "/login")).toHaveLength(2);
  });
});

// What the admin screens read stays in memory until the session's end empties the cache, which
// is what keeps it from a lower role after a role change: the change revokes every session of
// the user (ADR-0020, S87).
describe("the admin screens' reads at a session's end (S87)", () => {
  const ADMIN = me("admin", "afkadmin");
  const EMPTY: AuditPage = { items: [], unreadable: [] };

  /** How many cached queries the admin screens' keys hold. */
  function held(app: { runtime: { queryClient: QueryClient } }): number {
    const cache = app.runtime.queryClient.getQueryCache();
    return [AUDIT_KEY, AGENTS_KEY, USERS_KEY].flatMap((queryKey) => cache.findAll({ queryKey }))
      .length;
  }

  /** An Admin on the audit log, its entries and the users' names read. */
  async function onAudit(handlers: Parameters<typeof atScreen>[1] = {}) {
    const app = await atScreen(
      "/admin/audit",
      { audit: async () => EMPTY, users: async () => ({ items: [] }), ...handlers },
      { user: ADMIN },
    );
    await screen.findByRole("heading", { name: "Audit log", level: 1 });
    await waitFor(() => expect(held(app)).toBe(2));
    return app;
  }

  it("leaves none after a call's no_session", async () => {
    const app = await onAudit({ sessions: NO_SESSION });

    await act(() => app.runtime.apiClient.sessions().catch(() => {}));

    await waitFor(() => expect(app.path()).toBe("/login"));
    await waitFor(() => expect(held(app)).toBe(0));
  });

  it("leaves none after the live events' no_session", async () => {
    const watch = deferred<void>();
    const app = await onAudit({ watchEvents: () => watch.promise });

    await act(async () => watch.reject(fakeApiError("no_session")));

    await waitFor(() => expect(app.path()).toBe("/login"));
    await waitFor(() => expect(held(app)).toBe(0));
  });

  it("leaves none after a logout", async () => {
    const app = await onAudit({ logout: async () => ({ server: "ended" }) });

    await app.user.click(screen.getByRole("button", { name: "Log out" }));

    await waitFor(() => expect(app.path()).toBe("/login"));
    await waitFor(() => expect(held(app)).toBe(0));
  });

  it("builds nothing from a read that answers after the end", async () => {
    const late = deferred<AuditPage>();
    const app = await atScreen(
      "/admin/audit",
      { audit: () => late.promise, users: async () => ({ items: [] }), sessions: NO_SESSION },
      { user: ADMIN },
    );
    await waitFor(() => expect(held(app)).toBe(2));

    await act(() => app.runtime.apiClient.sessions().catch(() => {}));
    await waitFor(() => expect(app.path()).toBe("/login"));
    await waitFor(() => expect(held(app)).toBe(0));
    await act(async () => late.resolve({ items: [], unreadable: [] }));

    expect(held(app)).toBe(0);
  });

  it("shows a user back as a Member nothing of it, and reads none of it", async () => {
    let role: "admin" | "member" = "admin";
    const app = await onAudit({
      sessions: NO_SESSION,
      me: async () => me(role, "afkadmin"),
    });
    await act(() => app.runtime.apiClient.sessions().catch(() => {}));
    await waitFor(() => expect(app.path()).toBe("/login"));
    role = "member";
    const reads = app.fake.calls.filter((call) => call.method === "audit").length;

    act(() => app.runtime.stores.session.getState().signIn());
    await act(() => app.runtime.router.navigate({ to: "/admin/audit" }));

    expect(await screen.findByText(NOT_FOUND)).toBeDefined();
    expect(app.fake.calls.filter((call) => call.method === "audit")).toHaveLength(reads);
    expect(held(app)).toBe(0);
  });
});
