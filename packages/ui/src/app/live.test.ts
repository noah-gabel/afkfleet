import {
  type InfiniteData,
  InfiniteQueryObserver,
  QueryObserver,
  type QueryObserverOptions,
} from "@tanstack/react-query";
import { afterEach, describe, expect, it } from "vitest";

import type { ApiClient } from "../api/ApiClient";
import type { ApiError } from "../api/ApiError";
import { FakeApiClient, fakeApiError, fakeWatch } from "../api/FakeApiClient";
import {
  ACCOUNT_LIST_KEY,
  AWAITING_KEY,
  accountKey,
  accountQuery,
  accountsQuery,
  awaitingQuery,
  BOTS_LIST_KEY,
  grantsKey,
} from "../api/queries";
import type { AccountPage } from "../generated/AccountPage";
import type { AccountResponse } from "../generated/AccountResponse";
import type { LiveUpdateMessage } from "../generated/LiveUpdateMessage";
import { account, accountId } from "../test/accounts";
import { deferred } from "../test/fixtures";
import { accountFrame, bot, botFrame, disconnected, pages, refetch } from "../test/live";
import { createLiveLayer, HOLD_MAX } from "./live";
import { createQueryClient } from "./queryClient";
import { createAppStores } from "./stores";

/** What a test attached or subscribed, undone after it. */
const undo: (() => void)[] = [];

afterEach(() => {
  for (const step of undo.splice(0)) {
    step();
  }
});

/** A layer over a fake with `handlers` (a fake watch unless they script one), attached, signed in. */
function setup(handlers: Partial<ApiClient> = {}) {
  const watch = fakeWatch();
  const fake = new FakeApiClient({ watchEvents: watch.handler, ...handlers });
  const stores = createAppStores();
  const queryClient = createQueryClient();
  const layer = createLiveLayer(fake, queryClient, stores);
  undo.push(layer.attach());
  stores.session.getState().signIn();
  return { watch, fake, stores, queryClient, layer };
}

/** One watch a manual handler took, which nothing ends but the test. */
interface ManualCall {
  readonly push: (update: LiveUpdateMessage) => void;
  readonly end: () => void;
  readonly fail: (error: ApiError) => void;
}

/** A `watchEvents` handler whose watches only the test ends: an older one may still send. */
function manualWatch() {
  const calls: ManualCall[] = [];
  const handler: ApiClient["watchEvents"] = (onUpdate) =>
    new Promise<void>((resolve, reject) => {
      calls.push({ push: onUpdate, end: resolve, fail: reject });
    });
  return { handler, calls };
}

/** Shows `options`' query, as a mounted screen does: its invalidations read it again. */
function shown(client: ReturnType<typeof createQueryClient>, options: unknown) {
  const observer = new QueryObserver(client, options as QueryObserverOptions);
  undo.push(observer.subscribe(() => {}));
  return observer;
}

/** How many times `fake` was asked `method`. */
function asked(fake: FakeApiClient, method: keyof ApiClient): number {
  return fake.calls.filter((call) => call.method === method).length;
}

/** Lets the promises that are due run. */
async function settle(): Promise<void> {
  for (let turn = 0; turn < 10; turn += 1) {
    await Promise.resolve();
  }
  await new Promise((resolve) => setTimeout(resolve, 0));
}

/** The accounts of every page `data` holds. */
function listed(data: unknown): AccountResponse[] {
  return (data as InfiniteData<AccountPage> | undefined)?.pages.flatMap((page) => page.items) ?? [];
}

describe("the cache", () => {
  it("never creates an entry", () => {
    const { watch, queryClient } = setup();

    watch.latest().push(botFrame(bot(1)));

    expect(queryClient.getQueryCache().getAll()).toEqual([]);
  });

  it("follows a bot frame: the list and the account replaced, the grants read again, twice the same", () => {
    const { watch, queryClient } = setup();
    queryClient.setQueryData(ACCOUNT_LIST_KEY, pages({ items: [account(3)] }));
    queryClient.setQueryData(accountKey(accountId(3)), account(3));
    queryClient.setQueryData(grantsKey(accountId(3)), pages({ items: [] }));

    watch.latest().push(accountFrame(3, { name: "AfkBot33" }));
    watch.latest().push(accountFrame(3, { name: "AfkBot33" }));

    expect(listed(queryClient.getQueryData(ACCOUNT_LIST_KEY))).toEqual([
      account(3, { name: "AfkBot33" }),
    ]);
    expect(queryClient.getQueryData(accountKey(accountId(3)))).toEqual(
      account(3, { name: "AfkBot33" }),
    );
    expect(queryClient.getQueryState(grantsKey(accountId(3)))?.isInvalidated).toBe(true);
  });

  it("holds an event for a query's first load and applies it once the load succeeds", async () => {
    const answer = deferred<AccountPage>();
    const { watch, fake, queryClient } = setup({ accounts: () => answer.promise });

    const loading = queryClient.fetchInfiniteQuery(accountsQuery(fake));
    watch.latest().push(accountFrame(4, {}));
    answer.resolve({ items: [account(5), account(3)] });
    await loading;
    await settle();

    expect(listed(queryClient.getQueryData(ACCOUNT_LIST_KEY))).toEqual([
      account(5),
      account(4),
      account(3),
    ]);
  });

  it("holds an event during a refetch and applies it over the fresh data", async () => {
    const answer = deferred<AccountResponse>();
    const { watch, fake, queryClient } = setup({ account: () => answer.promise });
    queryClient.setQueryData(accountKey(accountId(3)), account(3));

    const refetching = queryClient.fetchQuery({
      ...accountQuery(fake, accountId(3)),
      staleTime: 0,
    });
    watch.latest().push(accountFrame(3, { name: "AfkBot33" }));
    answer.resolve(account(3, { name: "Read before the change" }));
    await refetching;
    await settle();

    expect(queryClient.getQueryData(accountKey(accountId(3)))).toEqual(
      account(3, { name: "AfkBot33" }),
    );
  });

  it("holds an event during Show more and applies it to the page it belongs to", async () => {
    const next = deferred<AccountPage>();
    const { watch, fake, queryClient } = setup({
      accounts: (before) =>
        before === undefined
          ? Promise.resolve({ items: [account(9), account(7)], next: accountId(7) })
          : next.promise,
    });
    const observer = new InfiniteQueryObserver(queryClient, accountsQuery(fake));
    undo.push(observer.subscribe(() => {}));
    await settle();

    const more = observer.fetchNextPage();
    watch.latest().push(accountFrame(2, { name: "AfkBot22" }));
    next.resolve({ items: [account(5), account(2)] });
    await more;
    await settle();

    expect(listed(queryClient.getQueryData(ACCOUNT_LIST_KEY))).toEqual([
      account(9),
      account(7),
      account(5),
      account(2, { name: "AfkBot22" }),
    ]);
  });

  it(`reads a query once more instead of replaying more than ${HOLD_MAX} held events`, async () => {
    const answer = deferred<AccountResponse>();
    const { watch, fake, queryClient } = setup({ account: () => answer.promise });
    queryClient.setQueryData(accountKey(accountId(3)), account(3));

    const refetching = queryClient.fetchQuery({
      ...accountQuery(fake, accountId(3)),
      staleTime: 0,
    });
    for (let n = 0; n <= HOLD_MAX; n += 1) {
      watch.latest().push(accountFrame(3, { name: `AfkBot${n}` }));
    }
    answer.resolve(account(3, { name: "Fetched" }));
    await refetching;
    await settle();

    const state = queryClient.getQueryState(accountKey(accountId(3)));
    expect(state?.data).toEqual(account(3, { name: "Fetched" }));
    expect(state?.isInvalidated).toBe(true);
  });

  it("a refetch drops every hold and reads everything again, wherever it lands", async () => {
    const answer = deferred<AccountResponse>();
    const { watch, fake, queryClient } = setup({ account: () => answer.promise });
    queryClient.setQueryData(accountKey(accountId(3)), account(3));
    queryClient.setQueryData(BOTS_LIST_KEY, pages({ items: [bot(1)] }));

    const refetching = queryClient.fetchQuery({
      ...accountQuery(fake, accountId(3)),
      staleTime: 0,
    });
    watch.latest().push(accountFrame(3, { name: "Held" }));
    watch.latest().push(refetch("resync"));
    answer.resolve(account(3, { name: "Fresh" }));
    await refetching;
    await settle();

    expect(queryClient.getQueryData(accountKey(accountId(3)))).toEqual(
      account(3, { name: "Fresh" }),
    );
    expect(queryClient.getQueryState(BOTS_LIST_KEY)?.isInvalidated).toBe(true);
  });
});

describe("a failed fetch (S63)", () => {
  it("applies its held events to the data it keeps, keeps its error, and reads nothing more", async () => {
    const answer = deferred<AccountResponse>();
    const { watch, fake, queryClient } = setup({ account: () => answer.promise });
    queryClient.setQueryData(accountKey(accountId(3)), account(3));

    const refetching = queryClient
      .fetchQuery({ ...accountQuery(fake, accountId(3)), staleTime: 0 })
      .catch(() => undefined);
    watch.latest().push(accountFrame(3, { name: "AfkBot33" }));
    answer.reject(fakeApiError("unreachable"));
    await refetching;
    await settle();
    const afterTheFailure = queryClient.getQueryState(accountKey(accountId(3)));
    watch.latest().push(accountFrame(3, { name: "AfkBot34" }));

    const state = queryClient.getQueryState(accountKey(accountId(3)));
    expect(afterTheFailure?.data).toEqual(account(3, { name: "AfkBot33" }));
    expect(state?.data).toEqual(account(3, { name: "AfkBot34" }));
    expect(state?.status).toBe("error");
    expect(state?.error).toBeTruthy();
    expect(asked(fake, "account")).toBe(1);
  });

  it("applies nothing after a failed first load, and creates no data", async () => {
    const answer = deferred<AccountResponse>();
    const { watch, fake, queryClient } = setup({ account: () => answer.promise });

    const loading = queryClient.fetchQuery(accountQuery(fake, accountId(3))).catch(() => undefined);
    watch.latest().push(accountFrame(3, {}));
    answer.reject(fakeApiError("unreachable"));
    await loading;
    await settle();

    expect(queryClient.getQueryData(accountKey(accountId(3)))).toBeUndefined();
  });

  it("applies a failed Show more's events to the pages it keeps, the error kept", async () => {
    const next = deferred<AccountPage>();
    const { watch, fake, queryClient } = setup({
      accounts: (before) =>
        before === undefined
          ? Promise.resolve({ items: [account(9), account(7)], next: accountId(7) })
          : next.promise,
    });
    const observer = new InfiniteQueryObserver(queryClient, accountsQuery(fake));
    undo.push(observer.subscribe(() => {}));
    await settle();

    const more = observer.fetchNextPage();
    watch.latest().push(accountFrame(9, { name: "AfkBot99" }));
    next.reject(fakeApiError("unreachable"));
    await more;
    await settle();

    expect(listed(queryClient.getQueryData(ACCOUNT_LIST_KEY))).toEqual([
      account(9, { name: "AfkBot99" }),
      account(7),
    ]);
    expect(observer.getCurrentResult().isFetchNextPageError).toBe(true);
  });

  it("reads nothing more for a held change that wanted a read", async () => {
    const answers: ReturnType<typeof deferred<AccountResponse[]>>[] = [];
    const { watch, fake, queryClient } = setup({
      accountsAwaitingApproval: async () => {
        const answer = deferred<AccountResponse[]>();
        answers.push(answer);
        return { items: await answer.promise };
      },
    });
    queryClient.setQueryData(AWAITING_KEY, [account(4, { status: "pending" })]);
    shown(queryClient, awaitingQuery(fake, true));
    await settle();

    watch.latest().push(accountFrame(3, { status: "pending" }));
    answers[0]?.reject(fakeApiError("unreachable"));
    await settle();

    expect(asked(fake, "accountsAwaitingApproval")).toBe(1);
  });

  it("drops a removed query's hold", async () => {
    const answer = deferred<AccountResponse>();
    const { watch, fake, queryClient } = setup({ account: () => answer.promise });
    queryClient.setQueryData(accountKey(accountId(3)), account(3));

    const refetching = queryClient
      .fetchQuery({ ...accountQuery(fake, accountId(3)), staleTime: 0 })
      .catch(() => undefined);
    watch.latest().push(accountFrame(3, { name: "Held" }));
    queryClient.clear();
    queryClient.setQueryData(accountKey(accountId(3)), account(3, { name: "New session" }));
    answer.resolve(account(3));
    await refetching;
    await settle();

    expect(queryClient.getQueryData(accountKey(accountId(3)))).toEqual(
      account(3, { name: "New session" }),
    );
  });
});

describe("the waiting list (S65)", () => {
  /** A fake whose waiting list answers `items`, its reads counted. */
  function listing(items: AccountResponse[]) {
    return setup({ accountsAwaitingApproval: async () => ({ items }) });
  }

  it("reads the list once for a pending link it doesn't list, which may stay off it", async () => {
    const own = account(3, { status: "pending" });
    const { watch, fake, queryClient } = listing([account(4, { status: "pending" })]);
    shown(queryClient, awaitingQuery(fake, true));
    await settle();

    watch.latest().push(accountFrame(3, { status: own.status }));
    await settle();

    expect(asked(fake, "accountsAwaitingApproval")).toBe(2);
    expect(queryClient.getQueryData(AWAITING_KEY)).toEqual([account(4, { status: "pending" })]);
  });

  it("reads nothing for frames of active bots", async () => {
    const { watch, fake, queryClient } = listing([account(4, { status: "pending" })]);
    shown(queryClient, awaitingQuery(fake, true));
    await settle();

    for (let n = 0; n < 200; n += 1) {
      watch.latest().push(accountFrame(n % 90, {}));
    }
    await settle();

    expect(asked(fake, "accountsAwaitingApproval")).toBe(1);
  });

  it("asks nothing for a Member, whose list is never read", async () => {
    const { watch, fake } = listing([]);

    watch.latest().push(accountFrame(3, { status: "pending" }));
    await settle();

    expect(asked(fake, "accountsAwaitingApproval")).toBe(0);
  });

  it("reads the list at most once more for a burst during its fetch", async () => {
    const answers: ReturnType<typeof deferred<AccountResponse[]>>[] = [];
    const { watch, fake, queryClient } = setup({
      accountsAwaitingApproval: async () => {
        const answer = deferred<AccountResponse[]>();
        answers.push(answer);
        return { items: await answer.promise };
      },
    });
    shown(queryClient, awaitingQuery(fake, true));
    await settle();

    for (let n = 1; n <= 20; n += 1) {
      watch.latest().push(accountFrame(n, { status: "pending" }));
    }
    answers[0]?.resolve([]);
    await settle();
    answers[1]?.resolve([]);
    await settle();

    expect(asked(fake, "accountsAwaitingApproval")).toBe(2);
  });
});

describe("whose updates count (S62)", () => {
  it("a replaced watch's updates change nothing", () => {
    const watch = manualWatch();
    const { stores, queryClient } = setup({ watchEvents: watch.handler });
    queryClient.setQueryData(BOTS_LIST_KEY, pages({ items: [bot(1)] }));
    stores.live.getState().restart();

    watch.calls[0]?.push(botFrame(bot(1, { desired_state: "running" })));
    watch.calls[0]?.push(refetch());

    expect(watch.calls).toHaveLength(2);
    expect(queryClient.getQueryData(BOTS_LIST_KEY)).toEqual(pages({ items: [bot(1)] }));
    expect(queryClient.getQueryState(BOTS_LIST_KEY)?.isInvalidated).toBe(false);
  });

  it("an older generation's watch changes nothing", () => {
    const watch = manualWatch();
    const { stores, queryClient } = setup({ watchEvents: watch.handler });
    stores.session.getState().signOut();
    stores.session.getState().signIn();
    queryClient.setQueryData(BOTS_LIST_KEY, pages({ items: [bot(1)] }));

    watch.calls[0]?.push(botFrame(bot(1, { desired_state: "running" })));

    expect(watch.calls).toHaveLength(2);
    expect(queryClient.getQueryData(BOTS_LIST_KEY)).toEqual(pages({ items: [bot(1)] }));
  });

  it("an old watch's hold isn't replayed", async () => {
    const watch = manualWatch();
    const answer = deferred<AccountResponse>();
    const { fake, stores, queryClient } = setup({
      watchEvents: watch.handler,
      account: () => answer.promise,
    });
    queryClient.setQueryData(accountKey(accountId(3)), account(3));

    const refetching = queryClient.fetchQuery({
      ...accountQuery(fake, accountId(3)),
      staleTime: 0,
    });
    watch.calls[0]?.push(accountFrame(3, { name: "Held" }));
    stores.live.getState().restart();
    answer.resolve(account(3, { name: "Fetched" }));
    await refetching;
    await settle();

    expect(queryClient.getQueryData(accountKey(accountId(3)))).toEqual(
      account(3, { name: "Fetched" }),
    );
  });

  it("a replaced watch's end starts nothing", async () => {
    const watch = manualWatch();
    const { stores, layer } = setup({ watchEvents: watch.handler });
    stores.live.getState().restart();

    watch.calls[0]?.fail(fakeApiError("certificate"));
    await settle();
    layer.recovered();

    expect(watch.calls).toHaveLength(2);
  });
});

describe("when a watch starts (S61, R73)", () => {
  it("one per sign-in, none after a stopped watch, none at a second attach", async () => {
    const { watch, layer } = setup();
    undo.push(layer.attach());

    watch.latest().end();
    await settle();

    expect(watch.calls).toHaveLength(1);
  });

  it("again after a save that changed nothing, and not after a sign-out", async () => {
    const { watch, stores } = setup();

    stores.live.getState().restart();
    stores.session.getState().signOut();
    await settle();

    expect(watch.calls).toHaveLength(2);
  });

  it("again once the connection recovers after a refused certificate, and only then", async () => {
    const { watch, layer } = setup();

    layer.recovered();
    watch.latest().fail(fakeApiError("certificate"));
    await settle();
    layer.recovered();
    layer.recovered();

    expect(watch.calls).toHaveLength(2);
  });

  it("not again after a session's end", async () => {
    const { watch, layer } = setup();

    watch.latest().fail(fakeApiError("no_session"));
    await settle();
    layer.recovered();

    expect(watch.calls).toHaveLength(1);
  });

  it("two layers over one fake stop after two watches", async () => {
    const watch = fakeWatch();
    const fake = new FakeApiClient({ watchEvents: watch.handler });
    for (let n = 0; n < 2; n += 1) {
      const stores = createAppStores();
      undo.push(createLiveLayer(fake, createQueryClient(), stores).attach());
      stores.session.getState().signIn();
    }
    await settle();

    expect(watch.calls).toHaveLength(2);
  });
});

describe("the Header's hint (U30)", () => {
  it("shows nothing for the routine reconnect, which doesn't wait", () => {
    const { watch, stores } = setup();

    watch.latest().push(disconnected(0));

    expect(stores.live.getState().hint).toEqual({ kind: "none" });
  });

  it("waits while a reconnect does, and goes at a sign-out", () => {
    const { watch, stores } = setup();

    watch.latest().push(disconnected(10));
    const waiting = stores.live.getState().hint;
    stores.session.getState().signOut();

    expect(waiting).toMatchObject({ kind: "waiting", secs: 10, tooMany: false });
    expect(stores.live.getState().hint).toEqual({ kind: "none" });
  });

  it("says the watch stopped after a refused certificate, until a new watch starts", async () => {
    const { watch, stores, layer } = setup();

    watch.latest().fail(fakeApiError("certificate"));
    await settle();
    const stopped = stores.live.getState().hint;
    layer.recovered();

    expect(stopped).toMatchObject({ kind: "stopped", error: { code: "certificate" } });
    expect(stores.live.getState().hint).toEqual({ kind: "none" });
  });
});
