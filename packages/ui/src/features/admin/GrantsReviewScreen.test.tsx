// The Owner's grants review (Plan.md P11.9; ADR-0018, ADR-0020, U79, S50, R178): the grants a user
// holds first, dormant ones marked, then their accounts a grant shares, each account's grants read
// only when opened, every revoke through the existing route, never asking for the password
// (ADR-0028); and the review
// offered on the users' screen, on every row the Owner may review and after a role change.
import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { ACCOUNTS_KEY, USERS_KEY } from "../../api/queries";
import type { HeldGrantResponse } from "../../generated/HeldGrantResponse";
import type { UserPatchRequest } from "../../generated/UserPatchRequest";
import type { UserResponse } from "../../generated/UserResponse";
import type { UserRole } from "../../generated/UserRole";
import { formatTime } from "../../lib/time";
import { account, accountId, BOB_ID, grant, ME_ID } from "../../test/accounts";
import { auditId, listedUser } from "../../test/audit";
import { deferred, me } from "../../test/fixtures";
import { atScreen, signedIn } from "../../test/signedIn";
import { refusal } from "../../test/signIn";
import { NOBODY_ELSE } from "../accounts/texts";

const OWNER = { ...me("owner", "afkowner"), id: ME_ID };
const REVIEWED = BOB_ID;
const CAROL = { id: auditId(3), username: "carol" };
const DAVE = { id: auditId(4), username: "dave" };

/** A grant Bob holds on account `n`. */
function held(n: number, changes: Partial<HeldGrantResponse> = {}): HeldGrantResponse {
  return {
    account: { id: accountId(n), name: `AfkBot${n}` },
    owner: CAROL,
    level: "view",
    granted_by: CAROL,
    granted_at: "2026-10-15T10:00:00.000Z",
    dormant: false,
    ...changes,
  };
}

/** The review of Bob, as `user` (the Owner by default), over `handlers`. */
async function atReview(handlers: Partial<ApiClient> = {}, user = OWNER) {
  const app = await signedIn(
    {
      users: async () => ({ items: [listedUser(REVIEWED, "bob")] }),
      userGrants: async () => ({ items: [] }),
      sharedAccounts: async () => ({ items: [] }),
      ...handlers,
    },
    { user },
  );
  await act(() =>
    app.runtime.router.navigate({
      to: "/admin/users/$id/grants",
      params: { id: REVIEWED },
    }),
  );
  return app;
}

/** The section named `name`. */
async function section(name: string) {
  return within(await screen.findByRole("region", { name }));
}

describe("the grants review", () => {
  it("names the user, with the way back", async () => {
    await atReview();

    expect(await screen.findByRole("heading", { name: "Grants of bob", level: 1 })).toBeDefined();
    expect(screen.getByRole("button", { name: "Back" })).toBeDefined();
  });

  it("shows the user's ID when the names can't be read", async () => {
    await atReview({
      users: async () => {
        throw refusal(500, "internal_error");
      },
    });

    expect(
      await screen.findByRole("heading", { name: `Grants of ${REVIEWED}`, level: 1 }),
    ).toBeDefined();
  });

  it("says when there's nothing to review", async () => {
    await atReview();

    expect((await section("Grants they hold")).getByText("No grants held.")).toBeDefined();
    expect(
      (await section("Their accounts others can use")).getByText("None shared."),
    ).toBeDefined();
  });

  it("lists the grants they hold, marking a dormant one", async () => {
    await atReview({
      userGrants: async () => ({
        items: [held(1, { level: "control" }), held(2, { level: "manage", dormant: true })],
      }),
    });

    const heldGrants = await section("Grants they hold");
    const active = within(await heldGrants.findByRole("row", { name: /AfkBot1/ }));
    expect(active.getByText("Control")).toBeDefined();
    expect(active.getAllByText("carol")).toHaveLength(2);
    expect(active.getByText(formatTime("2026-10-15T10:00:00.000Z"))).toBeDefined();
    expect(active.queryByText(/^Dormant/)).toBeNull();
    const dormant = within(heldGrants.getByRole("row", { name: /AfkBot2/ }));
    expect(dormant.getByText("Manage")).toBeDefined();
    expect(
      dormant.getByText(
        "Dormant: their role gives Manage here. Counts again if they become a Member or the owner an Admin.",
      ),
    ).toBeDefined();
  });

  it("marks a dormant grant on a shared account, naming whose role gives it (S90)", async () => {
    const app = await atReview({
      sharedAccounts: async () => ({ items: [account(1)] }),
      accountGrants: async () => ({
        items: [
          { ...grant("control", CAROL), dormant: true },
          { ...grant("view", DAVE), dormant: false },
        ],
      }),
    });
    const shared = await section("Their accounts others can use");

    await app.user.click(
      await shared.findByRole("button", { name: "Show who has access to AfkBot1" }),
    );

    const carol = within(await shared.findByRole("row", { name: /carol/ }));
    expect(
      carol.getByText(
        "Dormant: carol's role gives Manage here. Counts again if carol becomes a Member or the owner an Admin.",
      ),
    ).toBeDefined();
    const dave = within(shared.getByRole("row", { name: /dave/ }));
    expect(dave.queryByText(/^Dormant/)).toBeNull();
  });

  it("says nobody else has access when an opened account's grants are gone", async () => {
    const app = await atReview({
      sharedAccounts: async () => ({ items: [account(1)] }),
      accountGrants: async () => ({ items: [] }),
    });
    const shared = await section("Their accounts others can use");

    await app.user.click(
      await shared.findByRole("button", { name: "Show who has access to AfkBot1" }),
    );

    expect(await shared.findByText(NOBODY_ELSE)).toBeDefined();
  });

  it("reads a shared account's grants only when it's opened, and hides them again", async () => {
    const accountGrants = vi.fn(async () => ({ items: [grant("control", CAROL)] }));
    const app = await atReview({
      sharedAccounts: async () => ({ items: [account(1), account(2)] }),
      accountGrants,
    });
    const shared = await section("Their accounts others can use");
    await shared.findByText("AfkBot1");

    expect(accountGrants).not.toHaveBeenCalled();
    await app.user.click(shared.getByRole("button", { name: "Show who has access to AfkBot1" }));

    expect(await shared.findByRole("cell", { name: "carol" })).toBeDefined();
    expect(shared.getByRole("cell", { name: "Control" })).toBeDefined();
    expect(accountGrants).toHaveBeenCalledTimes(1);
    expect(accountGrants).toHaveBeenCalledWith(accountId(1), undefined, undefined);

    await app.user.click(shared.getByRole("button", { name: "Hide who has access to AfkBot1" }));

    expect(shared.queryByRole("cell", { name: "carol" })).toBeNull();
  });

  it("pages each list with Show more", async () => {
    const userGrants = vi.fn(async (_id: string, before?: string) =>
      before === undefined ? { items: [held(2)], next: accountId(2) } : { items: [held(1)] },
    );
    const sharedAccounts = vi.fn(async (_id: string, before?: string) =>
      before === undefined ? { items: [account(4)], next: accountId(4) } : { items: [account(3)] },
    );
    const app = await atReview({ userGrants, sharedAccounts });
    const heldGrants = await section("Grants they hold");
    const shared = await section("Their accounts others can use");

    await app.user.click(await heldGrants.findByRole("button", { name: "Show more" }));
    await app.user.click(await shared.findByRole("button", { name: "Show more" }));

    expect(await heldGrants.findByRole("row", { name: /AfkBot1/ })).toBeDefined();
    expect(await shared.findByText("AfkBot3")).toBeDefined();
    expect(userGrants).toHaveBeenLastCalledWith(REVIEWED, accountId(2), undefined);
    expect(sharedAccounts).toHaveBeenLastCalledWith(REVIEWED, accountId(4), undefined);
  });

  it("puts its two sections side by side from an 830 px box (U124, PR #92's review)", async () => {
    await atReview();
    await section("Grants they hold");

    const both = document.querySelector("[data-slot=review-sections]");
    expect(both?.className).toContain("@min-[51.875rem]/content:grid-cols-2");
    expect(both?.children).toHaveLength(2);
  });

  it("says each list loads, as a status", async () => {
    await atReview({
      userGrants: () => new Promise(() => {}),
      sharedAccounts: () => new Promise(() => {}),
    });

    expect(
      (await section("Grants they hold")).getByText("Loading grants…").getAttribute("role"),
    ).toBe("status");
    expect(
      (await section("Their accounts others can use"))
        .getByText("Loading accounts…")
        .getAttribute("role"),
    ).toBe("status");
  });

  it("says which list couldn't be read", async () => {
    await atReview({
      userGrants: async () => {
        throw refusal(500, "internal");
      },
      sharedAccounts: async () => {
        throw refusal(500, "internal");
      },
    });

    expect((await section("Grants they hold")).getByRole("alert").textContent).toMatch(
      /^Couldn't load grants: /,
    );
    expect((await section("Their accounts others can use")).getByRole("alert").textContent).toMatch(
      /^Couldn't load accounts: /,
    );
  });

  it("shows the server's refusal where a list can't be read", async () => {
    await atReview({
      userGrants: async () => {
        throw refusal(403, "forbidden", { message: "Only the Owner reviews grants." });
      },
    });

    expect((await section("Grants they hold")).getByRole("alert").textContent).toContain(
      "Only the Owner reviews grants.",
    );
  });

  describe("revoking", () => {
    /** A stateful fake: Bob's one held grant, gone after a revoke. No `stepUp`: a call fails. */
    function revocable() {
      let grants = [held(1)];
      const revokeGrant = vi.fn(async () => {
        grants = [];
      });
      return {
        revokeGrant,
        handlers: {
          userGrants: async () => ({ items: grants }),
          revokeGrant,
        } satisfies Partial<ApiClient>,
      };
    }

    it("asks first, never for the password, through the existing route, and reads again", async () => {
      const { revokeGrant, handlers } = revocable();
      const app = await atReview(handlers);
      const heldGrants = await section("Grants they hold");

      await app.user.click(
        await heldGrants.findByRole("button", { name: "Revoke bob's access to AfkBot1" }),
      );
      expect(heldGrants.getByText("Revoke bob's access to AfkBot1?")).toBeDefined();
      await app.user.click(heldGrants.getByRole("button", { name: "Revoke" }));

      expect(await screen.findByText("bob no longer has access.")).toBeDefined();
      expect(screen.queryByLabelText("Current password")).toBeNull();
      expect(revokeGrant).toHaveBeenCalledTimes(1);
      expect(revokeGrant).toHaveBeenLastCalledWith(accountId(1), REVIEWED);
      expect(await heldGrants.findByText("No grants held.")).toBeDefined();
    });

    it("sends nothing on Cancel", async () => {
      const { revokeGrant, handlers } = revocable();
      const app = await atReview(handlers);
      const heldGrants = await section("Grants they hold");

      await app.user.click(
        await heldGrants.findByRole("button", { name: "Revoke bob's access to AfkBot1" }),
      );
      await app.user.click(heldGrants.getByRole("button", { name: "Cancel" }));

      expect(revokeGrant).not.toHaveBeenCalled();
      expect(heldGrants.queryByText("Revoke bob's access to AfkBot1?")).toBeNull();
    });

    it("revokes someone's grant on a shared account, and that account leaves the list", async () => {
      let grants = [grant("view", CAROL)];
      const revokeGrant = vi.fn(async () => {
        grants = [];
      });
      const app = await atReview({
        sharedAccounts: async () => ({ items: grants.length === 0 ? [] : [account(1)] }),
        accountGrants: async () => ({ items: grants }),
        revokeGrant,
      });
      const shared = await section("Their accounts others can use");

      await app.user.click(
        await shared.findByRole("button", { name: "Show who has access to AfkBot1" }),
      );
      await app.user.click(
        await shared.findByRole("button", { name: "Revoke carol's access to AfkBot1" }),
      );
      await app.user.click(shared.getByRole("button", { name: "Revoke" }));

      expect(revokeGrant).toHaveBeenCalledWith(accountId(1), CAROL.id);
      expect(await shared.findByText("None shared.")).toBeDefined();
    });

    it("takes a revoked row away when the account keeps a second grantee", async () => {
      let grants = [grant("view", CAROL), grant("view", DAVE)];
      const app = await atReview({
        sharedAccounts: async () => ({ items: [account(1)] }),
        accountGrants: async () => ({ items: grants }),
        revokeGrant: async () => {
          grants = [grant("view", DAVE)];
        },
      });
      const shared = await section("Their accounts others can use");

      await app.user.click(
        await shared.findByRole("button", { name: "Show who has access to AfkBot1" }),
      );
      await app.user.click(
        await shared.findByRole("button", { name: "Revoke carol's access to AfkBot1" }),
      );
      await app.user.click(shared.getByRole("button", { name: "Revoke" }));

      await waitFor(() => expect(shared.queryByRole("row", { name: /carol/ })).toBeNull());
      expect(shared.getByRole("row", { name: /dave/ })).toBeDefined();
    });

    it("shows a refusal in place and reads the lists again", async () => {
      const userGrants = vi.fn(async () => ({ items: [held(1)] }));
      const app = await atReview({
        userGrants,
        revokeGrant: async () => {
          throw refusal(404, "not_found", { message: "It's gone." });
        },
      });
      const heldGrants = await section("Grants they hold");

      await app.user.click(
        await heldGrants.findByRole("button", { name: "Revoke bob's access to AfkBot1" }),
      );
      const reads = userGrants.mock.calls.length;
      await app.user.click(heldGrants.getByRole("button", { name: "Revoke" }));

      expect((await heldGrants.findByRole("alert")).textContent).toContain("It's gone.");
      await waitFor(() => expect(userGrants.mock.calls.length).toBeGreaterThan(reads));
    });
  });
});

describe("the review at a session's end (ADR-0020, S87)", () => {
  it("builds nothing from a revoke that answers after the end", async () => {
    const late = deferred<void>();
    const app = await atReview({
      userGrants: async () => ({ items: [held(1)] }),
      revokeGrant: () => late.promise,
    });
    const heldGrants = await section("Grants they hold");
    await app.user.click(
      await heldGrants.findByRole("button", { name: "Revoke bob's access to AfkBot1" }),
    );
    await app.user.click(heldGrants.getByRole("button", { name: "Revoke" }));

    await act(async () => {
      await app.runtime.router.navigate({ to: "/login" });
      app.runtime.queryClient.clear();
    });
    await act(async () => late.resolve());

    const cache = app.runtime.queryClient.getQueryCache();
    expect(cache.findAll({ queryKey: USERS_KEY })).toEqual([]);
    expect(cache.findAll({ queryKey: ACCOUNTS_KEY })).toEqual([]);
  });
});

describe("the users' screen's way to the review", () => {
  /** Bob and an Admin, with every role change answered, as `user`. */
  async function atUsers(user = OWNER) {
    let bob: UserResponse = listedUser(REVIEWED, "bob");
    return atScreen(
      "/admin/users",
      {
        users: async () => ({
          items: [
            bob,
            { ...listedUser(auditId(4), "owner2"), role: "owner" as UserRole },
            { ...listedUser(user.id, user.username), role: user.role },
          ],
        }),
        patchUser: async (_id: string, request: UserPatchRequest) => {
          bob = { ...bob, ...request };
          return bob;
        },
        userGrants: async () => ({ items: [] }),
        sharedAccounts: async () => ({ items: [] }),
      },
      { user },
    );
  }

  it("offers Review grants to the Owner on a Member or an Admin, never on themselves", async () => {
    await atUsers();

    expect(await screen.findByRole("link", { name: "Review grants of bob" })).toBeDefined();
    expect(screen.queryByRole("link", { name: "Review grants of afkowner" })).toBeNull();
    expect(screen.queryByRole("link", { name: "Review grants of owner2" })).toBeNull();
  });

  it("offers it to no Admin", async () => {
    await atUsers(me("admin", "afkadmin"));

    await screen.findByRole("link", { name: "Audit of bob" });
    expect(screen.queryByRole("link", { name: /^Review grants/ })).toBeNull();
  });

  it("opens the review from the row", async () => {
    const app = await atUsers();

    await app.user.click(await screen.findByRole("link", { name: "Review grants of bob" }));

    expect(await screen.findByRole("heading", { name: "Grants of bob", level: 1 })).toBeDefined();
  });

  it("stays on the screen after a promotion, offering the review (U79)", async () => {
    const app = await atUsers();
    const row = within(await screen.findByRole("row", { name: /^bob/ }));

    await app.user.click(row.getByRole("button", { name: "Make admin" }));
    await app.user.click(
      screen.getAllByRole("button", { name: "Make admin" }).at(-1) as HTMLElement,
    );

    expect(await screen.findByText("bob now an Admin. Earlier grants stay.")).toBeDefined();
    expect(app.path()).toBe("/admin/users");
    const offer = screen.getByText("bob now an Admin. Earlier grants stay.")
      .parentElement as HTMLElement;
    await app.user.click(within(offer).getByRole("link", { name: "Review grants" }));
    expect(await screen.findByRole("heading", { name: "Grants of bob", level: 1 })).toBeDefined();
  });

  it("says what a demotion brings back", async () => {
    let bob: UserResponse = { ...listedUser(REVIEWED, "bob"), role: "admin" };
    const app = await atScreen(
      "/admin/users",
      {
        users: async () => ({ items: [bob] }),
        patchUser: async (_id: string, request: UserPatchRequest) => {
          bob = { ...bob, ...request };
          return bob;
        },
      },
      { user: OWNER },
    );
    const row = within(await screen.findByRole("row", { name: /^bob/ }));

    await app.user.click(row.getByRole("button", { name: "Make member" }));
    await app.user.click(
      screen.getAllByRole("button", { name: "Make member" }).at(-1) as HTMLElement,
    );

    expect(await screen.findByText("bob now a Member. Their grants count again.")).toBeDefined();
  });

  it("offers nothing after disabling someone", async () => {
    const app = await atUsers();
    const row = within(await screen.findByRole("row", { name: /^bob/ }));

    await app.user.click(row.getByRole("button", { name: "Disable" }));
    await app.user.click(screen.getAllByRole("button", { name: "Disable" }).at(-1) as HTMLElement);

    await screen.findByText("bob disabled.");
    expect(screen.queryByText(/Grants/)).toBeNull();
  });
});
