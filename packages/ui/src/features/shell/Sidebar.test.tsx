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

describe("the sidebar", () => {
  it("has no header, and shows the user and their role in its Account box", async () => {
    await signedIn(async () => ({ server: "ended" }));

    expect(screen.queryByRole("banner")).toBeNull();
    const account = screen.getByRole("group", { name: "Account" });
    expect(within(account).getByText("ownername")).toBeDefined();
    expect(within(account).getByText("Owner")).toBeDefined();
  });

  it("opens Profile from the box, whose name holds what it shows", async () => {
    const app = await signedIn(async () => ({ server: "ended" }));

    const account = screen.getByRole("group", { name: "Account" });
    await app.user.click(within(account).getByRole("link", { name: "ownername, Owner, Profile" }));

    await waitFor(() => expect(app.path()).toBe("/profile"));
  });

  it("keeps Profile in the list once, beside the box, and Log out outside the list", async () => {
    await signedIn(async () => ({ server: "ended" }));

    const nav = screen.getByRole("navigation", { name: "Main" });
    expect(within(nav).getAllByRole("link", { name: /Profile/ })).toHaveLength(1);
    expect(within(nav).queryByRole("button", { name: "Log out" })).toBeNull();
    expect(
      within(screen.getByRole("group", { name: "Account" })).getByRole("button", {
        name: "Log out",
      }),
    ).toBeDefined();
  });

  it("names Log out's tooltip, and no title, as it's only an icon", async () => {
    const app = await signedIn(async () => ({ server: "ended" }));

    const button = screen.getByRole("button", { name: "Log out" });
    await app.user.hover(button);

    expect(document.querySelector("[data-slot=tooltip]")?.textContent).toBe("Log out");
    expect(button.getAttribute("title")).toBeNull();
  });

  it("lists Admin's entries under their heading, and only for an Admin or the Owner", async () => {
    await signedIn(async () => ({ server: "ended" }));

    const nav = screen.getByRole("navigation", { name: "Main" });
    expect(within(nav).getByText("Admin")).toBeDefined();
    expect(within(nav).getByRole("link", { name: "Audit log" })).toBeDefined();
  });

  it("gives every entry a name in the DOM, so the collapsed form keeps it", async () => {
    await signedIn(async () => ({ server: "ended" }));

    const nav = screen.getByRole("navigation", { name: "Main" });
    expect(
      within(nav)
        .getAllByRole("link")
        .map((link) => link.textContent),
    ).toEqual([
      "Home",
      "Modes",
      "Accounts",
      "Server",
      "Profile",
      "Users",
      "Invites",
      "Minecraft servers",
      "Agents",
      "Audit log",
    ]);
  });

  it("names Accounts with what's waiting, as a Member's list hides the rest", async () => {
    const app = renderApp({
      serverProfile: async () => PROFILE,
      me: async () => me("member", "afkbot1"),
    });
    await onHome("afkbot1");

    const nav = screen.getByRole("navigation", { name: "Main" });
    expect(within(nav).queryByText("Admin")).toBeNull();
    expect(within(nav).getByRole("link", { name: "Accounts" })).toBeDefined();
    expect(app.path()).toBe("/home");
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
        "Logged out here. Server not told (The server can't be reached.): the session may stay valid until it expires.",
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
