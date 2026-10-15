import { screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { fakeApiError } from "../../api/FakeApiClient";
import type { AccountResponse } from "../../generated/AccountResponse";
import { formatTime } from "../../lib/time";
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
    expect(card("AfkBot1").getByText("Microsoft · Active · Manage")).toBeDefined();
    expect(
      card("AfkBot1").getByText(`Linked by you on ${formatTime("2026-10-15T10:00:00.000Z")}`),
    ).toBeDefined();
    expect(card("AfkBot2").getByText("Offline · Active · Manage")).toBeDefined();
    expect(card("AfkBot2").getByText(/^Linked by bob on /)).toBeDefined();
    expect(card("AfkBot3").getByText("Microsoft · Active · View")).toBeDefined();
    expect(card("AfkBot3").queryByText(/^Linked by/)).toBeNull();
    expect(card("AfkBot1").getByRole("link", { name: "Open AfkBot1" })).toBeDefined();
    expect(screen.getByRole("link", { name: "Link Microsoft account" })).toBeDefined();
  });

  it("opens an account's screen from its card", async () => {
    const app = await accountsScreen({
      accounts: async () => ({ items: [account(1)] }),
      account: async () => account(1),
    });

    await app.user.click(await screen.findByRole("link", { name: "Open AfkBot1" }));

    expect(app.path()).toBe(`/accounts/${accountId(1)}`);
  });

  it("says so when there are no accounts", async () => {
    await accountsScreen({ accounts: async () => ({ items: [] }) });

    expect(
      await screen.findByText("No accounts yet. Link a Microsoft account to add one."),
    ).toBeDefined();
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
    /** The screen of an Owner with Bob's two pending links, which step-up when asked. */
    async function reviewer(handlers: Partial<ApiClient> = {}, stepUpFirst = false) {
      let stamped = !stepUpFirst;
      let waiting = [bobsPending(3), bobsPending(4)];
      const app = await accountsScreen(
        {
          accounts: async () => ({ items: waiting }),
          accountsAwaitingApproval: async () => ({ items: waiting }),
          stepUp: async () => {
            stamped = true;
          },
          approveAccount: async (id) => {
            if (!stamped) {
              throw refusal(403, "step_up_required");
            }
            waiting = waiting.filter((item) => item.id !== id);
            return account(3, { status: "active" });
          },
          refuseAccount: async (id) => {
            if (!stamped) {
              throw refusal(403, "step_up_required");
            }
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
      expect(waiting.getByText(/^Linked by bob on /)).toBeDefined();
      expect(waiting.getByRole("button", { name: "Approve AfkBot3" })).toBeDefined();
      expect(waiting.getByRole("button", { name: "Refuse AfkBot3" })).toBeDefined();
      expect(screen.getByRole("heading", { name: "Accounts you can see" })).toBeDefined();
      expect(card("AfkBot3").getByText("Microsoft · Waiting for approval · Manage")).toBeDefined();
    });

    it("approves after asking, with Confirm it's you when the server wants it", async () => {
      const app = await reviewer({}, true);
      const waiting = card("AfkBot3", "Waiting for your approval");

      await app.user.click(waiting.getByRole("button", { name: "Approve AfkBot3" }));
      expect(
        waiting.getByText("Approve AfkBot3, linked by bob? Bots can use it from then on."),
      ).toBeDefined();
      expect(called(app, "approveAccount")).toBe(0);
      await app.user.click(waiting.getByRole("button", { name: "Approve" }));
      await app.user.type(await screen.findByLabelText("Current password"), "placeholder password");
      await app.user.click(screen.getByRole("button", { name: "Continue" }));

      expect(await screen.findByText(approved("AfkBot3"))).toBeDefined();
      expect(called(app, "approveAccount")).toBe(2);
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
      await app.user.click(await screen.findByRole("button", { name: "Add an offline account" }));
      await app.user.type(screen.getByLabelText("Minecraft name"), "AfkBot9");
      return app;
    }

    it("is added from a form behind its own button, then listed", async () => {
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
      await app.user.click(await screen.findByRole("button", { name: "Add an offline account" }));
      expect(
        screen.getByRole("heading", { name: "Offline account (test servers only)" }),
      ).toBeDefined();
      await app.user.type(screen.getByLabelText("Minecraft name"), "AfkBot9");
      await app.user.click(screen.getByRole("button", { name: "Add" }));

      expect(await screen.findByText("AfkBot9 added.")).toBeDefined();
      expect(await screen.findByRole("heading", { name: "AfkBot9" })).toBeDefined();
      expect(screen.queryByLabelText("Minecraft name")).toBeNull();
      expect(app.fake.calls.find((call) => call.method === "createOfflineAccount")?.args).toEqual([
        { name: "AfkBot9" },
      ]);
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
      await app.user.click(await screen.findByRole("button", { name: "Add an offline account" }));
      await app.user.type(screen.getByLabelText("Minecraft name"), "no spaces");

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
