import { act, fireEvent, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { PAGE_MAX } from "../../api/limits";
import type { AccountResponse } from "../../generated/AccountResponse";
import { account, accountId, BOB_ID } from "../../test/accounts";
import { me } from "../../test/fixtures";
import { onHome, signedIn } from "../../test/signedIn";
import { refusal } from "../../test/signIn";

/** Bob's pending link `n`. */
function pending(n: number): AccountResponse {
  return account(n, { status: "pending", owner: { id: BOB_ID, username: "bob" } });
}

function reads(app: { fake: { calls: { method: string }[] } }): number {
  return app.fake.calls.filter((call) => call.method === "accountsAwaitingApproval").length;
}

/** Home for `user`, with `waiting` links waiting for them. */
async function home(
  waiting: AccountResponse[],
  user = me("owner", "owner"),
  handlers: Partial<ApiClient> = {},
) {
  return signedIn(
    { accountsAwaitingApproval: async () => ({ items: waiting }), ...handlers },
    { user },
  );
}

describe("the links waiting for approval", () => {
  it("are counted on Home and on the nav's Accounts, for the Owner", async () => {
    const app = await home([pending(1), pending(2)]);

    expect(
      await screen.findByText("2 Microsoft links are waiting for your approval."),
    ).toBeDefined();
    expect(
      screen.getByRole("link", { name: "Accounts, 2 waiting for your approval" }),
    ).toBeDefined();
    await app.user.click(screen.getByRole("link", { name: "Review" }));
    expect(app.path()).toBe("/accounts");
  });

  it("say one link in the singular, for an Admin", async () => {
    await home([pending(1)], me("admin", "admin"));

    expect(await screen.findByText("1 Microsoft link is waiting for your approval.")).toBeDefined();
  });

  it("show nothing when none wait", async () => {
    const app = await home([]);
    await vi.waitFor(() => expect(reads(app)).toBeGreaterThan(0));

    expect(screen.queryByText(/waiting for your approval/)).toBeNull();
    expect(screen.getByRole("link", { name: "Accounts" })).toBeDefined();
  });

  it("are never read for a Member, who sees neither", async () => {
    const app = await home([pending(1)], me());

    await onHome();
    expect(reads(app)).toBe(0);
    expect(screen.queryByText(/waiting for your approval/)).toBeNull();
  });

  it("are counted over every page, and the badge stops at 99+", async () => {
    const first = Array.from({ length: PAGE_MAX }, (_, n) => pending(n));
    const app = await signedIn(
      {
        accountsAwaitingApproval: async (before) =>
          before === undefined ? { items: first, next: accountId(99) } : { items: [pending(100)] },
      },
      { user: me("owner", "owner") },
    );

    expect(
      await screen.findByText("101 Microsoft links are waiting for your approval."),
    ).toBeDefined();
    expect(
      screen.getByRole("link", { name: "Accounts, 101 waiting for your approval" }).textContent,
    ).toContain("99+");
    expect(
      app.fake.calls
        .filter((call) => call.method === "accountsAwaitingApproval")
        .map((call) => call.args),
    ).toEqual([
      [undefined, PAGE_MAX],
      [accountId(99), PAGE_MAX],
    ]);
  });

  it("say so on Home when they can't be read, and show no badge", async () => {
    await home([], me("owner", "owner"), {
      accountsAwaitingApproval: async () => {
        throw refusal(500, "internal");
      },
    });

    expect(
      await screen.findByText(
        /^Couldn't check for links waiting for your approval: refused: internal \(request /,
      ),
    ).toBeDefined();
    expect(screen.getByRole("link", { name: "Accounts" })).toBeDefined();
  });

  it("are read again when the window regains focus, and never on a timer", async () => {
    let waiting = [pending(1)];
    const app = await home([], me("owner", "owner"), {
      accountsAwaitingApproval: async () => ({ items: waiting }),
    });
    await screen.findByText("1 Microsoft link is waiting for your approval.");
    const before = reads(app);
    waiting = [pending(1), pending(2)];

    act(() => {
      fireEvent.blur(window);
      fireEvent.focus(window);
    });

    expect(
      await screen.findByText("2 Microsoft links are waiting for your approval."),
    ).toBeDefined();
    expect(reads(app)).toBe(before + 1);
  });

  it("leave Home to the bots: no placeholder", async () => {
    await home([]);

    expect(await screen.findByRole("heading", { name: "Bots" })).toBeDefined();
    expect(screen.queryByText("Bots arrive in a later version.")).toBeNull();
  });
});
