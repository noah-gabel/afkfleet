// The universal Back control (Plan.md P11.11; ADR-0020, U111, U112, S126), over the router's memory
// history: one screen back, by the button, the mouse's side-button or Alt+Left, never past the first
// screen of this sign-in and never into the sign-in screens.
import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { account, accountId, seenBelowManage } from "../../test/accounts";
import { deferred, me } from "../../test/fixtures";
import { bot, botId } from "../../test/live";
import { atAccount, atScreen, onHome, signedIn } from "../../test/signedIn";
import { CODES, ENROLLMENT, enterCode, PASSWORD, refusal } from "../../test/signIn";

const ID = accountId(1);
const BOT = botId(1);

const backButton = () => screen.queryByRole("button", { name: "Back" });

/** The mouse's back side-button, pressed and let go. */
function sideButton(): void {
  act(() => {
    document.body.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, button: 3 }));
    document.body.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, button: 3 }));
  });
}

/** The App signed in, on bot 1's screen, opened from Home. */
async function homeThenBot(handlers: Partial<ApiClient> = {}) {
  const app = await signedIn({
    bots: async () => ({ items: [bot(1)] }),
    bot: async () => bot(1),
    ...handlers,
  });
  await app.user.click(await screen.findByRole("link", { name: "Open AfkBot1" }));
  await waitFor(() => expect(app.path()).toBe(`/bots/${BOT}`));
  await screen.findByRole("heading", { name: "AfkBot1", level: 1 });
  return app;
}

describe("the Back control", () => {
  it("isn't on the first screen after sign-in, and its row keeps its place", async () => {
    await signedIn();

    expect(backButton()).toBeNull();
    expect(document.querySelector("[data-slot=back-row]")).not.toBeNull();
  });

  it("goes from a bot back to Home", async () => {
    const app = await homeThenBot();

    await app.user.click(await screen.findByRole("button", { name: "Back" }));

    await waitFor(() => expect(app.path()).toBe("/home"));
    await waitFor(() => expect(backButton()).toBeNull());
  });

  it("goes back by the mouse's side-button", async () => {
    const app = await homeThenBot();

    sideButton();

    await waitFor(() => expect(app.path()).toBe("/home"));
  });

  it("goes back by Alt+Left", async () => {
    const app = await homeThenBot();

    await app.user.keyboard("{Alt>}{ArrowLeft}{/Alt}");

    await waitFor(() => expect(app.path()).toBe("/home"));
  });

  it("goes back by Alt+Left with the focus in a text field", async () => {
    const app = await atScreen("/modes/new");
    await app.user.click(await screen.findByLabelText("Name"));

    await app.user.keyboard("{Alt>}{ArrowLeft}{/Alt}");

    await waitFor(() => expect(app.path()).toBe("/home"));
  });

  it("does nothing on a plain Left, or with another modifier held", async () => {
    const app = await homeThenBot();

    await app.user.keyboard("{ArrowLeft}");
    await app.user.keyboard("{Control>}{Alt>}{ArrowLeft}{/Alt}{/Control}");

    expect(app.path()).toBe(`/bots/${BOT}`);
  });

  it("takes the side-button's event from the browser, even with nothing to go back to", async () => {
    await signedIn();
    const event = new MouseEvent("mouseup", { bubbles: true, cancelable: true, button: 3 });

    act(() => {
      document.body.dispatchEvent(event);
    });

    expect(event.defaultPrevented).toBe(true);
  });

  it("leaves the other mouse buttons alone", async () => {
    const app = await homeThenBot();
    const event = new MouseEvent("mouseup", { bubbles: true, cancelable: true, button: 1 });

    act(() => {
      document.body.dispatchEvent(event);
    });

    expect(event.defaultPrevented).toBe(false);
    expect(app.path()).toBe(`/bots/${BOT}`);
  });

  it("stops at the first screen however often it's pressed", async () => {
    const app = await homeThenBot();

    for (let press = 0; press < 5; press += 1) {
      sideButton();
    }

    await waitFor(() => expect(app.path()).toBe("/home"));
    await onHome();
    expect(backButton()).toBeNull();
  });

  it("keeps the audit's filter", async () => {
    const app = await signedIn(
      { audit: async () => ({ items: [], unreadable: [] }) },
      { user: me("owner") },
    );
    const target = { kind: "user", id: accountId(2) };
    await act(() => app.runtime.router.navigate({ to: "/admin/audit", search: target }));
    await act(() => app.runtime.router.navigate({ to: "/modes" }));

    await app.user.click(await screen.findByRole("button", { name: "Back" }));

    await waitFor(() => expect(app.path()).toBe("/admin/audit"));
    expect(app.runtime.router.state.location.search).toEqual(target);
  });

  it("puts the focus on the screen's heading after going back", async () => {
    const app = await homeThenBot();

    await app.user.click(await screen.findByRole("button", { name: "Back" }));

    await waitFor(() =>
      expect(document.activeElement).toBe(screen.getByRole("heading", { name: "Bots", level: 1 })),
    );
  });

  it("offers no way back after logging out and in again, and can't reach the login", async () => {
    const app = await homeThenBot({ logout: async () => ({ server: "ended" }) });
    await app.user.click(screen.getByRole("button", { name: "Log out" }));
    await screen.findByRole("heading", { name: "Log in" });
    act(() => app.runtime.stores.session.getState().signIn());
    await act(() => app.runtime.router.navigate({ to: "/home" }));
    await onHome();

    expect(backButton()).toBeNull();
    sideButton();
    await app.user.keyboard("{Alt>}{ArrowLeft}{/Alt}");

    expect(app.path()).toBe("/home");
  });

  it("does nothing while logging out", async () => {
    const logout = deferred<{ server: "ended" }>();
    const app = await homeThenBot({ logout: () => logout.promise });
    await app.user.click(screen.getByRole("button", { name: "Log out" }));

    sideButton();

    expect(app.path()).toBe(`/bots/${BOT}`);
    await act(async () => {
      logout.resolve({ server: "ended" });
      await logout.promise;
    });
  });

  it("has no way back on the new recovery codes step, whichever way it's asked", async () => {
    const app = await atScreen("/profile/2fa", {
      sessions: async () => ({ items: [] }),
      stepUp: async () => {},
      mfaReset: async () => ({ expires_in: 600 }),
      mfaEnroll: async () => ENROLLMENT,
      mfaConfirm: async () => ({ recovery_codes: CODES }),
    });
    expect(await screen.findByRole("button", { name: "Back" })).toBeDefined();
    await app.user.type(await screen.findByLabelText("Current password"), PASSWORD);
    await app.user.click(screen.getByRole("button", { name: "Continue" }));
    await enterCode(app, "654321");
    await screen.findByRole("list", { name: "Recovery codes" });

    expect(backButton()).toBeNull();
    expect(document.querySelector("[data-slot=back-row]")).not.toBeNull();
    sideButton();
    await app.user.keyboard("{Alt>}{ArrowLeft}{/Alt}");

    expect(app.path()).toBe("/profile/2fa");
    expect(screen.getByRole("list", { name: "Recovery codes" })).toBeDefined();
  });
});

describe("a screen left because what it showed is gone", () => {
  /** Home, then Accounts, then account 1's screen, as a grantee who may leave. */
  async function accountsThenAccount(handlers: Partial<ApiClient> = {}) {
    const seen = seenBelowManage(1, "view");
    const app = await atScreen("/accounts", {
      accounts: async () => ({ items: [seen] }),
      account: async () => seen,
      ...handlers,
    });
    await act(() => app.runtime.router.navigate({ to: "/accounts/$id", params: { id: ID } }));
    await screen.findByRole("heading", { name: seen.name, level: 1 });
    return app;
  }

  it("skips the gone account and the list's second entry after Leave", async () => {
    const app = await accountsThenAccount({ leaveAccount: async () => {} });
    await app.user.click(screen.getByRole("button", { name: "Leave" }));
    await app.user.click(screen.getByRole("button", { name: "Leave account" }));
    await waitFor(() => expect(app.path()).toBe("/accounts"));

    await app.user.click(await screen.findByRole("button", { name: "Back" }));

    await waitFor(() => expect(app.path()).toBe("/home"));
  });

  it("skips the gone account after a delete", async () => {
    const app = await atScreen("/accounts", {
      accounts: async () => ({ items: [account(1)] }),
      account: async () => account(1),
      deleteAccount: async () => {},
    });
    await act(() => app.runtime.router.navigate({ to: "/accounts/$id", params: { id: ID } }));
    await screen.findByRole("heading", { name: "AfkBot1", level: 1 });
    await app.user.click(screen.getByRole("button", { name: "Delete" }));
    await app.user.click(screen.getByRole("button", { name: "Delete account" }));
    await waitFor(() => expect(app.path()).toBe("/accounts"));

    await app.user.click(await screen.findByRole("button", { name: "Back" }));

    await waitFor(() => expect(app.path()).toBe("/home"));
  });

  it("doesn't move another screen when a Leave answers after the user went elsewhere", async () => {
    const leave = deferred<void>();
    const app = await accountsThenAccount({ leaveAccount: () => leave.promise });
    await app.user.click(screen.getByRole("button", { name: "Leave" }));
    await app.user.click(screen.getByRole("button", { name: "Leave account" }));
    await act(() => app.runtime.router.navigate({ to: "/modes" }));
    await within(screen.getByRole("main")).findByRole("link", { name: "New mode" });

    await act(async () => {
      leave.resolve();
      await leave.promise;
    });

    expect(app.path()).toBe("/modes");
  });

  it("shows the screen's own not-found state when Back lands on something gone meanwhile", async () => {
    const seen = seenBelowManage(1, "view");
    let there = true;
    const app = await atAccount(ID, {
      account: async () => {
        if (!there) {
          throw refusal(404, "not_found");
        }
        return seen;
      },
    });
    await screen.findByRole("heading", { name: seen.name, level: 1 });
    await act(() => app.runtime.router.navigate({ to: "/modes" }));
    there = false;
    app.runtime.queryClient.removeQueries();

    await app.user.click(await screen.findByRole("button", { name: "Back" }));

    expect((await screen.findByRole("alert")).textContent).toMatch(/can't see it|not available/);
    expect(app.path()).toBe(`/accounts/${ID}`);
  });
});
