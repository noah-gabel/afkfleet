import { screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { fakeApiError } from "../../api/FakeApiClient";
import type { AccountResponse } from "../../generated/AccountResponse";
import { account, accountId, BOB_ID, seenBelowManage } from "../../test/accounts";
import { me } from "../../test/fixtures";
import { atScreen } from "../../test/signedIn";
import { refusal } from "../../test/signIn";
import {
  ASK_MANAGER,
  approved,
  NO_LONGER_ACCEPTED,
  NOT_WAITING,
  OFFLINE_CONFLICT,
  OFFLINE_OFF,
  reasonText,
  refused,
} from "./texts";

/** Bob's pending link, as a reviewer sees it. */
function bobsPending(n: number): AccountResponse {
  return account(n, { status: "pending", owner: { id: BOB_ID, username: "bob" } });
}

/** The Accounts screen over `handlers`, signed in as `user`. */
async function accountsScreen(handlers: Partial<ApiClient>, user = me()) {
  const app = await atScreen("/accounts", handlers, { user });
  await screen.findByRole("heading", { name: "Accounts", level: 1 });
  return app;
}

/** The card of the account named `name`, in the list `list`. */
function card(name: string, list = "Accounts") {
  const heading = within(screen.getByRole("list", { name: list })).getByRole("heading", {
    name,
  });
  const item = heading.closest("li");
  if (item === null) {
    throw new Error(`${name} has no card`);
  }
  return within(item);
}

/** The labelled facts on `name`'s card, in order. */
function facts(name: string, list = "Accounts"): (string | null)[][] {
  const item = card(name, list);
  return item
    .getAllByRole("term")
    .map((term) => [term.textContent, term.nextElementSibling?.textContent ?? null]);
}

/** What `element`'s description reads. */
function description(element: HTMLElement): string | null | undefined {
  const id = element.getAttribute("aria-describedby");
  return id === null ? null : document.getElementById(id)?.textContent;
}

function called(app: { fake: { calls: { method: string }[] } }, method: string) {
  return app.fake.calls.filter((call) => call.method === method).length;
}

describe("the Accounts screen", () => {
  it("shows each account as a card with its type, status, access and who linked it", async () => {
    await accountsScreen({
      accounts: async () => ({
        items: [
          account(1),
          account(2, { kind: "offline", owner: { id: BOB_ID, username: "bob" } }),
          seenBelowManage(3, "view"),
        ],
      }),
    });

    const own = await screen.findByRole("heading", { name: "AfkBot1" });
    expect(own).toBeDefined();
    expect(facts("AfkBot1")).toEqual([
      ["Type", "Microsoft"],
      ["Your access", "Manage"],
      ["Linked by", "you"],
    ]);
    expect(card("AfkBot1").getByText("Active")).toBeDefined();
    expect(facts("AfkBot2")).toEqual([
      ["Type", "Offline"],
      ["Your access", "Manage"],
      ["Linked by", "bob"],
    ]);
    expect(facts("AfkBot3")).toEqual([
      ["Type", "Microsoft"],
      ["Your access", "View"],
    ]);
  });

  it("makes each card one link, named by the account, with no Open button", async () => {
    await accountsScreen({ accounts: async () => ({ items: [account(1)] }) });

    await screen.findByRole("heading", { name: "AfkBot1" });
    expect(card("AfkBot1").getByRole("link", { name: "AfkBot1" })).toBeDefined();
    expect(screen.queryByRole("link", { name: /^Open/ })).toBeNull();
  });

  it("offers both ways to add an account at the top, with a first account", async () => {
    await accountsScreen({ accounts: async () => ({ items: [account(1)] }) });

    await screen.findByRole("heading", { name: "AfkBot1" });
    const add = screen.getByRole("link", { name: "Add offline account" });
    expect(add.getAttribute("href")).toBe("/accounts/offline");
    expect(screen.getByRole("link", { name: "Link Microsoft account" }).getAttribute("href")).toBe(
      "/accounts/link",
    );
    expect(add.querySelector("svg")?.getAttribute("aria-hidden")).toBe("true");
    expect(screen.queryByRole("link", { name: "Microsoft account" })).toBeNull();
  });

  it("opens an account's screen from its card", async () => {
    const app = await accountsScreen({
      accounts: async () => ({ items: [account(1)] }),
      account: async () => account(1),
    });

    await app.user.click(await screen.findByRole("link", { name: "AfkBot1" }));

    expect(app.path()).toBe(`/accounts/${accountId(1)}`);
  });

  it("offers two tiles when there are no accounts, and the top's buttons hide", async () => {
    await accountsScreen({ accounts: async () => ({ items: [] }) });

    const microsoft = await screen.findByRole("link", { name: "Microsoft account" });
    expect(microsoft.getAttribute("href")).toBe("/accounts/link");
    expect(description(microsoft)).toBe("For online-mode servers. Signs in once with a code.");
    const offline = screen.getByRole("link", { name: "Offline account" });
    expect(offline.getAttribute("href")).toBe("/accounts/offline");
    expect(description(offline)).toBe("For offline-mode servers. Just a name.");
    expect(screen.queryByRole("link", { name: "Link Microsoft account" })).toBeNull();
    expect(screen.queryByRole("link", { name: "Add offline account" })).toBeNull();
  });

  it("shows neither the buttons nor the tiles while the list loads", async () => {
    await accountsScreen({ accounts: () => new Promise(() => {}) });

    expect(await screen.findByText("Loading accounts…")).toBeDefined();
    expect(screen.queryByRole("link", { name: "Link Microsoft account" })).toBeNull();
    expect(screen.queryByRole("link", { name: "Microsoft account" })).toBeNull();
  });

  it("keeps the buttons when the list can't be read, with no tiles", async () => {
    await accountsScreen({
      accounts: async () => {
        throw refusal(500, "internal");
      },
    });

    await screen.findByText(/refused: internal \(request /);
    expect(screen.getByRole("link", { name: "Link Microsoft account" })).toBeDefined();
    expect(screen.queryByRole("link", { name: "Microsoft account" })).toBeNull();
  });

  it("keeps the buttons for a reviewer with links waiting and none of their own", async () => {
    await accountsScreen(
      {
        accounts: async () => ({ items: [] }),
        accountsAwaitingApproval: async () => ({ items: [bobsPending(5)] }),
      },
      me("admin"),
    );

    await screen.findByRole("list", { name: "Waiting for your approval" });
    expect(await screen.findByRole("link", { name: "Link Microsoft account" })).toBeDefined();
    expect(screen.queryByRole("link", { name: "Microsoft account" })).toBeNull();
    expect(screen.getByText("No accounts.")).toBeDefined();
  });

  it("shows the list's failure", async () => {
    await accountsScreen({
      accounts: async () => {
        throw refusal(500, "internal");
      },
    });

    expect(await screen.findByText(/refused: internal \(request /)).toBeDefined();
  });

  it("asks a manager to link an account again that Microsoft no longer accepts", async () => {
    const app = await accountsScreen({
      accounts: async () => ({
        items: [
          account(1, { status: "reauth_required" }),
          account(2, { status: "refused", reason: "not_owned" }),
        ],
      }),
    });

    await screen.findByRole("heading", { name: "AfkBot1" });
    expect(card("AfkBot1").getByText(NO_LONGER_ACCEPTED)).toBeDefined();
    expect(card("AfkBot2").getByText(reasonText("not_owned", "AfkBot2"))).toBeDefined();
    expect(card("AfkBot1").queryByText(ASK_MANAGER, { exact: false })).toBeNull();
    await app.user.click(card("AfkBot1").getByRole("link", { name: "Link again" }));
    expect(app.path()).toBe(`/accounts/${accountId(1)}/link-again`);
  });

  it("asks a viewer below Manage to turn to whoever manages it, without a button", async () => {
    await accountsScreen({
      accounts: async () => ({
        items: [
          seenBelowManage(1, "control", { status: "reauth_required" }),
          seenBelowManage(2, "view", { status: "refused", reason: "xbox_banned" }),
        ],
      }),
    });

    await screen.findByRole("heading", { name: "AfkBot1" });
    expect(card("AfkBot1").getByText(`${NO_LONGER_ACCEPTED} ${ASK_MANAGER}`)).toBeDefined();
    expect(
      card("AfkBot2").getByText(`${reasonText("xbox_banned", "AfkBot2")} ${ASK_MANAGER}`),
    ).toBeDefined();
    expect(screen.queryByRole("link", { name: "Link again" })).toBeNull();
  });

  it("loads more accounts with Show more", async () => {
    const app = await accountsScreen({
      accounts: async (before) =>
        before === undefined
          ? { items: [account(2)], next: accountId(2) }
          : { items: [account(1)] },
    });

    await app.user.click(await screen.findByRole("button", { name: "Show more" }));

    expect(await screen.findByRole("heading", { name: "AfkBot1" })).toBeDefined();
    expect(
      app.fake.calls.find((call) => call.method === "accounts" && call.args[0] !== undefined)?.args,
    ).toEqual([accountId(2), undefined]);
    expect(screen.queryByRole("button", { name: "Show more" })).toBeNull();
  });

  it("never reads the waiting links for a Member, and shows no waiting section", async () => {
    const app = await accountsScreen({ accounts: async () => ({ items: [account(1)] }) });

    await screen.findByRole("heading", { name: "AfkBot1" });
    expect(called(app, "accountsAwaitingApproval")).toBe(0);
    expect(screen.queryByRole("heading", { name: "Waiting for your approval" })).toBeNull();
  });

  describe("the waiting section", () => {
    /** The screen of an Owner with Bob's two pending links; no `stepUp`, so a call fails. */
    async function reviewer(handlers: Partial<ApiClient> = {}) {
      let waiting = [bobsPending(3), bobsPending(4)];
      const app = await accountsScreen(
        {
          accounts: async () => ({ items: waiting }),
          accountsAwaitingApproval: async () => ({ items: waiting }),
          approveAccount: async (id) => {
            waiting = waiting.filter((item) => item.id !== id);
            return account(3, { status: "active" });
          },
          refuseAccount: async (id) => {
            waiting = waiting.filter((item) => item.id !== id);
          },
          ...handlers,
        },
        me("owner", "owner"),
      );
      await screen.findByRole("heading", { name: "Waiting for your approval" });
      return app;
    }

    it("lists the links waiting for the reviewer, with who linked them, above the rest", async () => {
      await reviewer();

      const waiting = card("AfkBot3", "Waiting for your approval");
      expect(facts("AfkBot3", "Waiting for your approval")).toContainEqual(["Linked by", "bob"]);
      expect(waiting.getByRole("button", { name: "Approve AfkBot3" })).toBeDefined();
      expect(waiting.getByRole("button", { name: "Refuse AfkBot3" })).toBeDefined();
      expect(screen.getByRole("heading", { name: "Accounts you can see" })).toBeDefined();
      expect(card("AfkBot3").getByText("Waiting for approval")).toBeDefined();
    });

    it("approves after asking, never for the password (ADR-0028)", async () => {
      const app = await reviewer();
      const waiting = card("AfkBot3", "Waiting for your approval");

      await app.user.click(waiting.getByRole("button", { name: "Approve AfkBot3" }));
      expect(
        waiting.getByText("Approve AfkBot3, linked by bob? Bots can use it from then on."),
      ).toBeDefined();
      expect(called(app, "approveAccount")).toBe(0);
      await app.user.click(waiting.getByRole("button", { name: "Approve" }));

      expect(await screen.findByText(approved("AfkBot3"))).toBeDefined();
      expect(screen.queryByLabelText("Current password")).toBeNull();
      expect(called(app, "approveAccount")).toBe(1);
      await vi.waitFor(() => expect(screen.queryByRole("heading", { name: "AfkBot3" })).toBeNull());
    });

    it("refuses after asking", async () => {
      const app = await reviewer();
      const waiting = card("AfkBot4", "Waiting for your approval");

      await app.user.click(waiting.getByRole("button", { name: "Refuse AfkBot4" }));
      expect(
        waiting.getByText("Refuse AfkBot4? The link is deleted, and bob can link it again."),
      ).toBeDefined();
      await app.user.click(waiting.getByRole("button", { name: "Refuse" }));

      expect(await screen.findByText(refused("AfkBot4"))).toBeDefined();
      expect(app.fake.calls.find((call) => call.method === "refuseAccount")?.args).toEqual([
        accountId(4),
      ]);
    });

    it("says a link isn't waiting any more when the server answers 409, and reads again", async () => {
      const app = await reviewer({
        approveAccount: async () => {
          throw refusal(409, "conflict");
        },
      });
      const reads = called(app, "accountsAwaitingApproval");
      const waiting = card("AfkBot3", "Waiting for your approval");

      await app.user.click(waiting.getByRole("button", { name: "Approve AfkBot3" }));
      await app.user.click(waiting.getByRole("button", { name: "Approve" }));

      expect(await waiting.findByText(NOT_WAITING)).toBeDefined();
      await vi.waitFor(() =>
        expect(called(app, "accountsAwaitingApproval")).toBeGreaterThan(reads),
      );
    });
  });

  describe("an offline account", () => {
    async function offlineForm(createOfflineAccount: ApiClient["createOfflineAccount"]) {
      const app = await accountsScreen({
        accounts: async () => ({ items: [] }),
        createOfflineAccount,
      });
      expect(screen.queryByLabelText("Minecraft name")).toBeNull();
      await app.user.click(await screen.findByRole("link", { name: "Offline account" }));
      await app.user.type(await screen.findByLabelText("Minecraft name"), "AfkBot9");
      return app;
    }

    it("is added on a screen of its own, then listed", async () => {
      let items: AccountResponse[] = [];
      const app = await atScreen(
        "/accounts",
        {
          accounts: async () => ({ items }),
          createOfflineAccount: async (request) => {
            items = [account(9, { kind: "offline", name: request.name })];
            return { id: accountId(9), name: request.name, kind: "offline", status: "active" };
          },
        },
        {},
      );
      await app.user.click(await screen.findByRole("link", { name: "Offline account" }));
      expect(await screen.findByRole("heading", { name: "Add an offline account" })).toBeDefined();
      expect(app.path()).toBe("/accounts/offline");
      await app.user.type(screen.getByLabelText("Minecraft name"), "AfkBot9");
      await app.user.click(screen.getByRole("button", { name: "Add" }));

      expect(await screen.findByText("AfkBot9 added.")).toBeDefined();
      expect(await screen.findByRole("heading", { name: "AfkBot9" })).toBeDefined();
      expect(screen.queryByLabelText("Minecraft name")).toBeNull();
      expect(app.path()).toBe("/accounts");
      expect(app.fake.calls.find((call) => call.method === "createOfflineAccount")?.args).toEqual([
        { name: "AfkBot9" },
      ]);
    });

    it("goes back to the list from its way out", async () => {
      const app = await offlineForm(async () => {
        throw refusal(500, "internal");
      });

      await app.user.click(screen.getByRole("link", { name: "Back to accounts" }));

      await screen.findByRole("heading", { name: "Accounts", level: 1 });
      expect(app.path()).toBe("/accounts");
    });

    it("says when the server doesn't allow offline accounts", async () => {
      const app = await offlineForm(async () => {
        throw refusal(404, "not_found");
      });

      await app.user.click(screen.getByRole("button", { name: "Add" }));

      expect(await screen.findByText(OFFLINE_OFF)).toBeDefined();
    });

    it("says a taken name or a full quota in one text", async () => {
      const app = await offlineForm(async () => {
        throw refusal(409, "conflict");
      });

      await app.user.click(screen.getByRole("button", { name: "Add" }));

      expect(await screen.findByText(OFFLINE_CONFLICT)).toBeDefined();
    });

    it("checks the name before sending it", async () => {
      const app = await accountsScreen({ accounts: async () => ({ items: [] }) });
      await app.user.click(await screen.findByRole("link", { name: "Offline account" }));
      await app.user.type(await screen.findByLabelText("Minecraft name"), "no spaces");

      await app.user.click(screen.getByRole("button", { name: "Add" }));

      expect(await screen.findByText("must be 3 to 16 letters, digits or `_`")).toBeDefined();
      expect(called(app, "createOfflineAccount")).toBe(0);
    });

    it("shows the server's rule at the field", async () => {
      const app = await offlineForm(async () => {
        throw fakeApiError("api", {
          api: {
            status: 422,
            code: "validation_failed",
            message: "Some fields are invalid.",
            request_id: "r-1",
            fields: [{ path: "name", message: "must be 3 to 16 letters, digits or `_`" }],
          },
        });
      });

      await app.user.click(screen.getByRole("button", { name: "Add" }));

      expect(await screen.findByText("must be 3 to 16 letters, digits or `_`")).toBeDefined();
    });
  });
});
