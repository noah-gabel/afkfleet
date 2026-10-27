import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { fakeApiError } from "../../api/FakeApiClient";
import { SESSION_ENDED } from "../../app/leave";
import type { LogoutResponse } from "../../generated/LogoutResponse";
import { me, PROFILE } from "../../test/fixtures";
import { renderApp } from "../../test/renderApp";
import { onHome } from "../../test/signedIn";

async function signedIn(logout: () => Promise<LogoutResponse>, role = me("owner", "ownername")) {
  const app = renderApp({ serverProfile: async () => PROFILE, me: async () => role, logout });
  await onHome("ownername");
  return app;
}

describe("the header", () => {
  it("shows the user and their role", async () => {
    await signedIn(async () => ({ server: "ended" }));

    const header = screen.getByRole("banner");
    expect(within(header).getByText("ownername")).toBeDefined();
    expect(within(header).getByText("Owner")).toBeDefined();
  });

  it("logs out to the login screen and empties the cache", async () => {
    const app = await signedIn(async () => ({ server: "ended" }));

    await app.user.click(screen.getByRole("button", { name: "Log out" }));

    await waitFor(() => expect(app.path()).toBe("/login"));
    expect(app.runtime.queryClient.getQueryData(["me"])).toBeUndefined();
    expect(app.runtime.stores.session.getState().phase).toBe("signed_out");
    expect(screen.queryByText(SESSION_ENDED)).toBeNull();
  });

  it("says the server's session may live on when it couldn't be told", async () => {
    const app = await signedIn(async () => ({
      server: "unconfirmed",
      error: { code: "unreachable", message: "The server can't be reached." },
    }));

    await app.user.click(screen.getByRole("button", { name: "Log out" }));

    await waitFor(() => expect(app.path()).toBe("/login"));
    expect(
      await screen.findByText(
        "You're logged out on this PC, but the server couldn't be told (The server can't be reached.), so your session there may stay valid until it expires.",
      ),
    ).toBeDefined();
    expect(app.runtime.queryClient.getQueryData(["me"])).toBeUndefined();
  });

  it("stays signed in when the logout itself fails", async () => {
    const app = await signedIn(async () => {
      throw fakeApiError("interrupted");
    });

    await app.user.click(screen.getByRole("button", { name: "Log out" }));

    expect(await screen.findByText("Couldn't log out: fake interrupted Try again.")).toBeDefined();
    expect(app.path()).toBe("/home");
    expect(app.runtime.stores.session.getState()).toMatchObject({
      phase: "signed_in",
      generation: 1,
    });
    expect(app.runtime.queryClient.getQueryData(["me"])).toBeDefined();
  });

  it("logs out quietly even when a call answers no_session meanwhile", async () => {
    let answer: (value: LogoutResponse) => void = () => {};
    const app = renderApp({
      serverProfile: async () => PROFILE,
      me: async () => me(),
      sessions: async () => {
        throw fakeApiError("no_session");
      },
      logout: () =>
        new Promise((resolve) => {
          answer = resolve;
        }),
    });
    await onHome("afkbot1");

    await app.user.click(screen.getByRole("button", { name: "Log out" }));
    await app.runtime.apiClient.sessions().catch(() => {});
    answer({ server: "ended" });

    await waitFor(() => expect(app.path()).toBe("/login"));
    expect(screen.queryByText(SESSION_ENDED)).toBeNull();
  });
});
