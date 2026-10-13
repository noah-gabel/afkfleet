import { act, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { fakeApiError } from "../api/FakeApiClient";
import type { LogoutResponse } from "../generated/LogoutResponse";
import type { SessionPage } from "../generated/SessionPage";
import { deferred, me, PROFILE } from "../test/fixtures";
import { renderApp } from "../test/renderApp";
import { SESSION_ENDED } from "./leave";

const NO_SESSION = async (): Promise<never> => {
  throw fakeApiError("no_session");
};

/** The App, signed in on Home. */
async function signedIn(handlers: Parameters<typeof renderApp>[0] = {}) {
  const app = renderApp({ serverProfile: async () => PROFILE, me: async () => me(), ...handlers });
  await screen.findByText(/Signed in as afkbot1/);
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
