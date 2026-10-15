import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { fakeApiError } from "../api/FakeApiClient";
import type { UserRole } from "../generated/UserRole";
import { me, PROFILE } from "../test/fixtures";
import { renderApp } from "../test/renderApp";

const NOT_FOUND = "This page doesn't exist or isn't available to you.";

async function signedInAs(role: UserRole) {
  const app = renderApp({
    serverProfile: async () => PROFILE,
    me: async () => me(role),
    users: async () => ({ items: [] }),
    invites: async () => ({ items: [] }),
  });
  await screen.findByText(/Signed in as afkbot1/);
  return app;
}

function navLinks(): string[] {
  const nav = screen.getByRole("navigation", { name: "Main" });
  return within(nav)
    .getAllByRole("link")
    .map((link) => link.textContent ?? "");
}

describe("the router", () => {
  it("sends a signed-out user from a signed-in screen to login", async () => {
    const app = renderApp({
      serverProfile: async () => PROFILE,
      me: async () => {
        throw fakeApiError("no_session");
      },
    });
    await waitFor(() => expect(app.path()).toBe("/login"));

    await act(() => app.runtime.router.navigate({ to: "/home" }));

    await waitFor(() => expect(app.path()).toBe("/login"));
  });

  it.each([
    ["/login", "/home"],
    ["/register", "/home"],
    ["/setup", "/server"],
  ] as const)("sends a signed-in user from %s to %s", async (from, to) => {
    const app = await signedInAs("member");

    await act(() => app.runtime.router.navigate({ to: from }));

    await waitFor(() => expect(app.path()).toBe(to));
  });

  it("shows a Member Home, Accounts, Server and Profile, nothing of the admin's", async () => {
    await signedInAs("member");

    expect(navLinks()).toEqual(["Home", "Accounts", "Server", "Profile"]);
  });

  it.each<UserRole>(["admin", "owner"])("shows an %s the admin screens too", async (role) => {
    const app = await signedInAs(role);

    expect(navLinks()).toEqual(["Home", "Accounts", "Server", "Profile", "Users", "Invites"]);
    await app.user.click(screen.getByRole("link", { name: "Users" }));
    expect(await screen.findByRole("heading", { name: "Users", level: 1 })).toBeDefined();
    await app.user.click(screen.getByRole("link", { name: "Invites" }));
    expect(await screen.findByRole("heading", { name: "Invites", level: 1 })).toBeDefined();
  });

  it.each(["/admin/users", "/admin/invites"] as const)(
    "answers a Member on %s with not found",
    async (to) => {
      const app = await signedInAs("member");

      await act(() => app.runtime.router.navigate({ to }));

      expect(await screen.findByText(NOT_FOUND)).toBeDefined();
      expect(screen.queryByRole("heading", { name: /^(Users|Invites)$/ })).toBeNull();
      expect(app.fake.calls.map((call) => call.method)).not.toContain(
        to === "/admin/users" ? "users" : "invites",
      );
    },
  );

  it("gives a role this app doesn't know only a Member's rights", async () => {
    const app = await signedInAs("superuser" as UserRole);

    expect(navLinks()).toEqual(["Home", "Accounts", "Server", "Profile"]);
    await act(() => app.runtime.router.navigate({ to: "/admin/users" }));
    expect(await screen.findByText(NOT_FOUND)).toBeDefined();
  });

  it("leads from not found back Home", async () => {
    const app = await signedInAs("member");
    await act(() => app.runtime.router.navigate({ to: "/admin/users" }));

    await app.user.click(await screen.findByRole("link", { name: "Go to Home" }));

    await waitFor(() => expect(app.path()).toBe("/home"));
  });

  it("shows a route's error with the error screen", async () => {
    const app = await signedInAs("admin");
    app.runtime.queryClient.removeQueries({ queryKey: ["me"] });
    app.fake.calls.length = 0;
    Object.assign(app.fake, {
      me: async () => {
        throw fakeApiError("interrupted");
      },
    });

    await act(() => app.runtime.router.navigate({ to: "/admin/users" }));

    expect(await screen.findByText("Something went wrong in the app.")).toBeDefined();
    expect(screen.getByText("fake interrupted")).toBeDefined();
  });
});
