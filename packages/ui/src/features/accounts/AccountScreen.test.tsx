import { screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { fakeApiError, fakeWatch } from "../../api/FakeApiClient";
import { BOTS_LIST_KEY } from "../../api/queries";
import type { AccountResponse } from "../../generated/AccountResponse";
import type { GrantResponse } from "../../generated/GrantResponse";
import { formatTime } from "../../lib/time";
import { account, accountId, BOB_ID, grant, seenBelowManage } from "../../test/accounts";
import { deferred, me } from "../../test/fixtures";
import { fakeTime } from "../../test/renderApp";
import { expectWait, limited } from "../../test/retryWait";
import { atAccount } from "../../test/signedIn";
import { refusal } from "../../test/signIn";
import {
  ASK_MANAGER,
  LINK_IT_AGAIN,
  NO_LONGER_ACCEPTED,
  NOBODY_ELSE,
  NOT_FOUND,
  PENDING_NOT_SHARED,
  PENDING_NOTE,
  reasonText,
  THEN_LINK_AGAIN,
} from "./texts";

const ID = accountId(1);

/** The server's 422 for a name that can't be given a grant (U4 in ADR-0018). */
const UNGRANTABLE_TEXT =
  "can't be given a grant: unknown, disabled, or already has access; check the name";

/**
 * Account 1's screen over `handlers`, where every write needs a step-up the server stamps
 * once `stepUp` ran (unless `stamped`), and the grants start as `grants`.
 */
async function accountScreen(
  options: {
    readonly account?: AccountResponse;
    readonly grants?: GrantResponse[];
    readonly handlers?: Partial<ApiClient>;
    readonly user?: ReturnType<typeof me>;
    readonly stamped?: boolean;
    readonly advanceTimers?: (ms: number) => unknown;
  } = {},
) {
  let stamped = options.stamped ?? true;
  let grants = options.grants ?? [];
  const needs = () => {
    if (!stamped) {
      throw refusal(403, "step_up_required");
    }
  };
  const app = await atAccount(
    ID,
    {
      account: async () => options.account ?? account(1),
      accountGrants: async () => ({ items: grants }),
      stepUp: async () => {
        stamped = true;
      },
      createGrant: async (_id, request) => {
        needs();
        const made = grant(request.level, { id: BOB_ID, username: request.username });
        grants = [made];
        return made;
      },
      changeGrant: async (_id, _user, request) => {
        needs();
        grants = [grant(request.level)];
        return grant(request.level);
      },
      revokeGrant: async () => {
        needs();
        grants = [];
      },
      deleteAccount: async () => {
        needs();
      },
      ...options.handlers,
    },
    {
      ...(options.user === undefined ? {} : { user: options.user }),
      ...(options.advanceTimers === undefined ? {} : { advanceTimers: options.advanceTimers }),
    },
  );
  await screen.findByRole("heading", { name: (options.account ?? account(1)).name, level: 1 });
  return app;
}

function called(
  app: { fake: { calls: { method: string; args: readonly unknown[] }[] } },
  method: string,
) {
  return app.fake.calls.filter((call) => call.method === method);
}

/** The value next to `term` in the account's facts. */
function fact(term: string): string {
  const dt = screen.getByText(term, { selector: "dt" });
  return dt.nextElementSibling?.textContent ?? "";
}

describe("an account's screen", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("shows the account's facts as the viewer may see them", async () => {
    await accountScreen({ account: account(1, { owner: { id: BOB_ID, username: "bob" } }) });

    expect(fact("Type")).toBe("Microsoft");
    expect(fact("Status")).toBe("Active");
    expect(fact("Your access")).toBe("Manage");
    expect(fact("Linked by")).toBe("bob");
    expect(fact("Linked on")).toBe(formatTime("2026-10-15T10:00:00.000Z"));
  });

  it("leaves out the owner for a viewer below Manage, and offers no writes", async () => {
    await accountScreen({ account: seenBelowManage(1, "control") as AccountResponse });

    expect(fact("Your access")).toBe("Control");
    expect(screen.queryByText("Linked by", { selector: "dt" })).toBeNull();
    expect(screen.queryByRole("heading", { name: "Who has access" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Delete" })).toBeNull();
    expect(screen.queryByRole("link", { name: "Link again" })).toBeNull();
  });

  it("says an account it can't find doesn't exist or can't be seen", async () => {
    const app = await atAccount(ID, {
      account: async () => {
        throw refusal(404, "not_found");
      },
    });

    expect(await screen.findByText(NOT_FOUND)).toBeDefined();
    await app.user.click(screen.getByRole("link", { name: "Back to accounts" }));
    expect(app.path()).toBe("/accounts");
  });

  it("says so when the account goes while its screen is open and its read answers 404", async () => {
    const watch = fakeWatch();
    let gone = false;
    await atAccount(ID, {
      watchEvents: watch.handler,
      account: async () => {
        if (gone) {
          throw refusal(404, "not_found");
        }
        return account(1);
      },
    });
    await screen.findByText("AfkBot1");

    gone = true;
    watch.latest().push({
      update: "event",
      event: { type: "bot_removed", bot_id: "01900000-0000-7000-8000-000000000201" },
    });

    expect(await screen.findByText(NOT_FOUND)).toBeDefined();
  });

  it("says so when the account goes during its screen's first read (R87)", async () => {
    const watch = fakeWatch();
    const first = deferred<AccountResponse>();
    let reads = 0;
    await atAccount(ID, {
      watchEvents: watch.handler,
      account: () => {
        reads += 1;
        return reads === 1 ? first.promise : Promise.reject(refusal(404, "not_found"));
      },
    });

    watch.latest().push({
      update: "event",
      event: { type: "bot_removed", bot_id: "01900000-0000-7000-8000-000000000201" },
    });
    first.resolve(account(1));

    expect(await screen.findByText(NOT_FOUND)).toBeDefined();
    expect(reads).toBe(2);
  });

  it("notes that a pending link waits for approval", async () => {
    await accountScreen({ account: account(1, { status: "pending" }) });

    expect(screen.getByText(PENDING_NOTE)).toBeDefined();
    expect(screen.queryByRole("link", { name: "Link again" })).toBeNull();
  });

  it("lets a reviewer approve the pending link from its screen", async () => {
    const pending = account(1, { status: "pending", owner: { id: BOB_ID, username: "bob" } });
    const app = await accountScreen({
      account: pending,
      user: me("owner", "owner"),
      handlers: {
        accountsAwaitingApproval: async () => ({ items: [pending] }),
        approveAccount: async () => account(1),
      },
    });

    await app.user.click(await screen.findByRole("button", { name: "Approve AfkBot1" }));
    await app.user.click(screen.getByRole("button", { name: "Approve" }));

    expect(await screen.findByText("AfkBot1 approved.")).toBeDefined();
    expect(screen.queryByText(PENDING_NOTE)).toBeNull();
  });

  it("prompts a manager to link again an account Microsoft no longer accepts", async () => {
    const app = await accountScreen({ account: account(1, { status: "reauth_required" }) });

    expect(screen.getByText(`${NO_LONGER_ACCEPTED} ${LINK_IT_AGAIN}`)).toBeDefined();
    await app.user.click(screen.getByRole("link", { name: "Link again" }));
    expect(app.path()).toBe(`/accounts/${ID}/link-again`);
  });

  it("prompts with a refused account's reason", async () => {
    await accountScreen({
      account: account(1, { status: "refused", reason: "xbox_child_account" }),
    });

    expect(
      screen.getByText(`${reasonText("xbox_child_account", "AfkBot1")} ${THEN_LINK_AGAIN}`),
    ).toBeDefined();
    expect(screen.getAllByRole("link", { name: "Link again" })).toHaveLength(1);
  });

  it("asks a viewer below Manage to turn to whoever manages it", async () => {
    await accountScreen({
      account: seenBelowManage(1, "view", { status: "reauth_required" }) as AccountResponse,
    });

    expect(screen.getByText(`${NO_LONGER_ACCEPTED} ${ASK_MANAGER}`)).toBeDefined();
  });

  it("offers linking again for an active account", async () => {
    const app = await accountScreen();

    await app.user.click(screen.getByRole("link", { name: "Link again" }));

    expect(app.path()).toBe(`/accounts/${ID}/link-again`);
  });

  describe("who has access", () => {
    it("lists the grants", async () => {
      await accountScreen({ grants: [grant("control")] });

      const table = await screen.findByRole("table", { name: "Who has access" });
      const row = within(within(table).getByText("bob").closest("tr") as HTMLElement);
      expect((row.getByLabelText("Access of bob") as HTMLSelectElement).value).toBe("control");
      expect(row.getByText("afkbot1")).toBeDefined();
      expect(row.getByText(formatTime("2026-10-15T11:00:00.000Z"))).toBeDefined();
    });

    it("says when nobody else has access", async () => {
      await accountScreen();

      expect(await screen.findByText(NOBODY_ELSE)).toBeDefined();
    });

    it("shares by exact username after Confirm it's you", async () => {
      const app = await accountScreen({ stamped: false });

      await app.user.type(await screen.findByLabelText("Username"), "bob");
      await app.user.click(screen.getByRole("radio", { name: /^Control/ }));
      await app.user.click(screen.getByRole("button", { name: "Share" }));
      const password = await screen.findByLabelText("Current password");
      // A form inside a form would submit the outer one natively, reloading the app.
      expect(password.closest("form")?.parentElement?.closest("form")).toBeNull();
      await app.user.type(password, "placeholder password");
      await app.user.click(screen.getByRole("button", { name: "Continue" }));

      expect(await screen.findByText("AfkBot1 is shared with bob.")).toBeDefined();
      expect(called(app, "createGrant").map((call) => call.args)).toEqual([
        [ID, { username: "bob", level: "control" }],
        [ID, { username: "bob", level: "control" }],
      ]);
      expect(await screen.findByLabelText("Access of bob")).toBeDefined();
    });

    it("shows the server's 422 at the username", async () => {
      const app = await accountScreen({
        handlers: {
          createGrant: async () => {
            throw fakeApiError("api", {
              api: {
                status: 422,
                code: "validation_failed",
                message: "Some fields are invalid.",
                request_id: "r-1",
                fields: [{ path: "username", message: UNGRANTABLE_TEXT }],
              },
            });
          },
        },
      });

      await app.user.type(await screen.findByLabelText("Username"), "nobody");
      await app.user.click(screen.getByRole("button", { name: "Share" }));

      expect(await screen.findByText(UNGRANTABLE_TEXT)).toBeDefined();
    });

    it.each([
      ["active", "bob already has access. Change their level in the list."],
      ["pending", PENDING_NOT_SHARED],
    ] as const)("tells a 409 on a %s account", async (status, text) => {
      const app = await accountScreen({
        account: account(1, { status }),
        handlers: {
          createGrant: async () => {
            throw refusal(409, "conflict");
          },
        },
      });

      await app.user.type(await screen.findByLabelText("Username"), "bob");
      await app.user.click(screen.getByRole("button", { name: "Share" }));

      expect(await screen.findByText(text)).toBeDefined();
    });

    it("waits out the by-name limit's 429", async () => {
      const time = fakeTime();
      const app = await accountScreen({
        advanceTimers: time.advanceTimers,
        handlers: {
          createGrant: async () => {
            throw limited();
          },
        },
      });

      await app.user.type(await screen.findByLabelText("Username"), "bob");
      await app.user.click(screen.getByRole("button", { name: "Share" }));

      expect(
        await screen.findByText("You've tried too many names for now.", { exact: false }),
      ).toBeDefined();
      await expectWait(app, time, { button: "Share", method: "createGrant" });
    });

    it("changes a level as soon as it's picked", async () => {
      const app = await accountScreen({ grants: [grant("view")] });

      await app.user.selectOptions(await screen.findByLabelText("Access of bob"), "manage");

      expect(await screen.findByText("bob now has Manage.")).toBeDefined();
      expect(called(app, "changeGrant").map((call) => call.args)).toEqual([
        [ID, BOB_ID, { level: "manage" }],
      ]);
    });

    it.each([
      ["active", "bob already manages AfkBot1 through their role."],
      ["pending", PENDING_NOT_SHARED],
    ] as const)("tells a raise's 409 on a %s account", async (status, text) => {
      const app = await accountScreen({
        account: account(1, { status }),
        grants: [grant("view")],
        handlers: {
          changeGrant: async () => {
            throw refusal(409, "conflict");
          },
        },
      });

      await app.user.selectOptions(await screen.findByLabelText("Access of bob"), "manage");

      expect(await screen.findByText(text)).toBeDefined();
    });

    it("revokes after asking", async () => {
      const app = await accountScreen({ grants: [grant("control")] });

      await app.user.click(await screen.findByRole("button", { name: "Revoke bob's access" }));
      expect(screen.getByText("Revoke bob's access to AfkBot1?")).toBeDefined();
      expect(called(app, "revokeGrant")).toHaveLength(0);
      await app.user.click(screen.getByRole("button", { name: "Revoke" }));

      expect(await screen.findByText("bob no longer has access.")).toBeDefined();
      expect(called(app, "revokeGrant").map((call) => call.args)).toEqual([[ID, BOB_ID]]);
      expect(await screen.findByText(NOBODY_ELSE)).toBeDefined();
    });

    it("loads more grants with Show more", async () => {
      const app = await accountScreen({
        handlers: {
          accountGrants: async (_id, before) =>
            before === undefined
              ? { items: [grant("view")], next: BOB_ID }
              : {
                  items: [
                    grant("view", {
                      id: "01900000-0000-7000-8000-000000000003",
                      username: "carol",
                    }),
                  ],
                },
        },
      });

      await app.user.click(await screen.findByRole("button", { name: "Show more" }));

      expect(await screen.findByLabelText("Access of carol")).toBeDefined();
    });
  });

  describe("leaving (ADR-0020, U23, S51, S86)", () => {
    const leaver = { handlers: { accounts: async () => ({ items: [] }) } };

    it.each(["view", "control"] as const)("offers Leave to a %s holder", async (level) => {
      await accountScreen({ account: seenBelowManage(1, level), ...leaver });

      expect(screen.getByRole("button", { name: "Leave" })).toBeDefined();
    });

    it("offers no Leave at Manage, whatever gave it", async () => {
      await accountScreen({
        account: account(1, { owner: { id: BOB_ID, username: "bob" } }),
        ...leaver,
      });

      expect(screen.queryByRole("button", { name: "Leave" })).toBeNull();
    });

    it("asks first, saying what's lost, and Cancel sends nothing", async () => {
      const leaveAccount = vi.fn(async () => {});
      const app = await accountScreen({
        account: seenBelowManage(1, "view"),
        handlers: { leaveAccount },
      });

      await app.user.click(screen.getByRole("button", { name: "Leave" }));

      expect(
        screen.getByText(
          "Leave AfkBot1? You lose access to it and its bot. Only someone who manages it can give it back.",
        ),
      ).toBeDefined();
      await app.user.click(screen.getByRole("button", { name: "Cancel" }));
      expect(leaveAccount).not.toHaveBeenCalled();
    });

    it("leaves after Confirm it's you, then goes back and reads the accounts and bots again", async () => {
      let stamped = false;
      const leaveAccount = vi.fn(async () => {
        if (!stamped) {
          throw refusal(403, "step_up_required");
        }
      });
      const app = await accountScreen({
        account: seenBelowManage(1, "control"),
        handlers: {
          leaveAccount,
          stepUp: async () => {
            stamped = true;
          },
          accounts: async () => ({ items: [] }),
        },
      });
      const accountReads = () => called(app, "account").length;

      await app.user.click(screen.getByRole("button", { name: "Leave" }));
      await app.user.click(screen.getByRole("button", { name: "Leave account" }));
      await app.user.type(await screen.findByLabelText("Current password"), "placeholder password");
      const before = accountReads();
      await app.user.click(screen.getByRole("button", { name: "Continue" }));

      expect(await screen.findByText("You left AfkBot1.")).toBeDefined();
      await vi.waitFor(() => expect(app.path()).toBe("/accounts"));
      expect(leaveAccount).toHaveBeenCalledTimes(2);
      expect(leaveAccount).toHaveBeenLastCalledWith(ID);
      // It went back before anything read the account again, so its 404 never flashed.
      expect(accountReads()).toBe(before);
      // Home isn't showing, so its list is marked to be read when it next shows.
      expect(app.runtime.queryClient.getQueryState(BOTS_LIST_KEY)?.isInvalidated).toBe(true);
    });

    it("says access is already gone at a 404, and goes back too", async () => {
      const app = await accountScreen({
        account: seenBelowManage(1, "view"),
        handlers: {
          leaveAccount: async () => {
            throw refusal(404, "not_found");
          },
          accounts: async () => ({ items: [] }),
        },
      });

      await app.user.click(screen.getByRole("button", { name: "Leave" }));
      await app.user.click(screen.getByRole("button", { name: "Leave account" }));

      expect(await screen.findByText("You no longer have access to AfkBot1.")).toBeDefined();
      await vi.waitFor(() => expect(app.path()).toBe("/accounts"));
    });

    it("shows another refusal where it was asked, and stays", async () => {
      const app = await accountScreen({
        account: seenBelowManage(1, "view"),
        handlers: {
          leaveAccount: async () => {
            throw refusal(403, "forbidden", { message: "Not yours to leave." });
          },
        },
      });

      await app.user.click(screen.getByRole("button", { name: "Leave" }));
      await app.user.click(screen.getByRole("button", { name: "Leave account" }));

      expect((await screen.findByRole("alert")).textContent).toContain("Not yours to leave.");
      expect(app.path()).toBe(`/accounts/${ID}`);
    });
  });

  it("lets a Manage grantee revoke their own row, which ends on not found (S86)", async () => {
    const self = { id: me().id, username: me().username };
    let revoked = false;
    const app = await accountScreen({
      account: account(1, { owner: { id: BOB_ID, username: "bob" } }),
      grants: [grant("manage", self)],
      handlers: {
        account: async () => {
          if (revoked) {
            throw refusal(404, "not_found");
          }
          return account(1, { owner: { id: BOB_ID, username: "bob" } });
        },
        revokeGrant: async () => {
          revoked = true;
        },
      },
    });

    await app.user.click(
      await screen.findByRole("button", { name: `Revoke ${self.username}'s access` }),
    );
    await app.user.click(screen.getAllByRole("button", { name: "Revoke" }).at(-1) as HTMLElement);

    expect(await screen.findByText(NOT_FOUND)).toBeDefined();
    expect(called(app, "revokeGrant")[0]?.args).toEqual([ID, self.id]);
  });

  it("deletes after asking and Confirm it's you, then goes back to the list", async () => {
    const app = await accountScreen({
      stamped: false,
      handlers: { accounts: async () => ({ items: [] }) },
    });

    await app.user.click(screen.getByRole("button", { name: "Delete" }));
    expect(
      screen.getByText(
        "Delete AfkBot1? Its stored sign-in and every grant go with it. You can link it again later.",
      ),
    ).toBeDefined();
    await app.user.click(screen.getByRole("button", { name: "Delete account" }));
    await app.user.type(await screen.findByLabelText("Current password"), "placeholder password");
    await app.user.click(screen.getByRole("button", { name: "Continue" }));

    expect(await screen.findByText("AfkBot1 deleted.")).toBeDefined();
    await vi.waitFor(() => expect(app.path()).toBe("/accounts"));
    expect(called(app, "deleteAccount")).toHaveLength(2);
  });
});
