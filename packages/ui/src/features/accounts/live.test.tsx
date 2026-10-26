// The account screens over the live events (Plan.md P11.8; ADR-0020, S60, S65): what another
// device or the server changes shows without a refresh.
import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { fakeApiError, fakeWatch } from "../../api/FakeApiClient";
import type { AccountResponse } from "../../generated/AccountResponse";
import { account } from "../../test/accounts";
import { me } from "../../test/fixtures";
import { accountFrame } from "../../test/live";
import { atScreen } from "../../test/signedIn";
import { accountsLinkName } from "./components/WaitingAlert";

/** The Accounts link's name with one link waiting. */
const ONE_WAITING = accountsLinkName(1) ?? "";

describe("the accounts over the live events", () => {
  it("show a new account and the waiting count as the frames come, reading only what they must", async () => {
    const watch = fakeWatch();
    let waiting: AccountResponse[] = [account(2, { status: "pending" })];
    const app = await atScreen(
      "/accounts",
      {
        watchEvents: watch.handler,
        accounts: async () => ({ items: [account(2, { status: "pending" }), account(1)] }),
        accountsAwaitingApproval: async () => ({ items: waiting }),
      },
      { user: me("owner", "afkowner") },
    );
    const nav = screen.getByRole("navigation", { name: "Main" });
    expect(await within(nav).findByRole("link", { name: ONE_WAITING })).toBeDefined();
    const reads = () => app.fake.calls.filter((call) => call.method === "accountsAwaitingApproval");
    const before = reads().length;

    // Another device links an account: its bot's frame places it at the top.
    watch.latest().push(accountFrame(3, {}));
    expect(await screen.findByText("AfkBot3")).toBeDefined();
    // The pending link is approved elsewhere: the frame takes it off the count, no read.
    watch.latest().push(accountFrame(2, { status: "active" }));
    expect(await within(nav).findByRole("link", { name: "Accounts" })).toBeDefined();
    expect(reads()).toHaveLength(before);
    // A new pending link the list doesn't hold: only the server knows whose it is.
    waiting = [account(4, { status: "pending" })];
    watch.latest().push(accountFrame(4, { status: "pending" }));

    expect(await within(nav).findByRole("link", { name: ONE_WAITING })).toBeDefined();
    expect(reads()).toHaveLength(before + 1);
    expect(app.fake.calls.filter((call) => call.method === "accounts")).toHaveLength(1);
  });
});

describe("the live events' watch over a refused certificate (R73)", () => {
  it("raises the certificate banner, and starts again once a check gets through", async () => {
    const watch = fakeWatch();
    const app = await atScreen("/accounts", {
      watchEvents: watch.handler,
      accounts: async () => ({ items: [account(1)] }),
      checkServer: async () => undefined,
    });

    watch.latest().fail(fakeApiError("certificate"));
    await screen.findByRole("button", { name: "Retry now" });
    expect(app.runtime.stores.connection.getState().status).toBe("certificate");
    await app.user.click(screen.getByRole("button", { name: "Retry now" }));

    await waitFor(() => expect(watch.calls).toHaveLength(2));
    expect(app.runtime.stores.connection.getState().status).toBe("ok");
  });
});

describe("the live events' watch in StrictMode", () => {
  it("starts once per sign-in, though every effect runs twice", async () => {
    const watch = fakeWatch();
    await atScreen(
      "/accounts",
      {
        watchEvents: watch.handler,
        accounts: async () => ({ items: [account(1)] }),
      },
      { strict: true },
    );

    await screen.findByText("AfkBot1");

    expect(watch.calls).toHaveLength(1);
  });
});
