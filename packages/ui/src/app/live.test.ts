import {
  type InfiniteData,
  InfiniteQueryObserver,
  type QueryClient,
  QueryObserver,
  type QueryObserverOptions,
} from "@tanstack/react-query";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../api/ApiClient";
import type { ApiError } from "../api/ApiError";
import { FakeApiClient, fakeApiError, fakeWatch } from "../api/FakeApiClient";
import { PAGE_MAX } from "../api/limits";
import {
  ACCOUNT_LIST_KEY,
  AWAITING_KEY,
  accountKey,
  accountQuery,
  accountsQuery,
  awaitingQuery,
  BOTS_LIST_KEY,
  botKey,
  botQuery,
  botsQuery,
  chatKey,
  chatOutcomesKey,
  chatOutcomesQuery,
  grantsKey,
  MODES_LIST_KEY,
  modesQuery,
} from "../api/queries";
import type { AccountPage } from "../generated/AccountPage";
import type { AccountResponse } from "../generated/AccountResponse";
import type { BotPage } from "../generated/BotPage";
import type { LiveUpdateMessage } from "../generated/LiveUpdateMessage";
import { account, accountId } from "../test/accounts";
import { deferred } from "../test/fixtures";
import {
  accountFrame,
  bot,
  botFrame,
  botId,
  disconnected,
  event,
  line,
  mode,
  modeId,
  notOpened,
  pages,
  refetch,
} from "../test/live";
import { refusal } from "../test/signIn";
import { createLiveLayer, HOLD_MAX } from "./live";
import { createQueryClient } from "./queryClient";
import { createAppStores } from "./stores";
import { watched } from "./watched";

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

/** The args of every call `fake` got of `method`. */
function argsOf(fake: FakeApiClient, method: keyof ApiClient): (readonly unknown[])[] {
  return fake.calls.filter((call) => call.method === method).map((call) => call.args);
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

/** An `account` handler whose reads each wait for the test, in the order they were asked. */
function reads() {
  const answers: ReturnType<typeof deferred<AccountResponse>>[] = [];
  return {
    answers,
    account: () => {
      const answer = deferred<AccountResponse>();
      answers.push(answer);
      return answer.promise;
    },
  };
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

  it("drops a removed query's hold: the next query under its key replays nothing", async () => {
    const { answers, account: read } = reads();
    const { watch, fake, queryClient } = setup({ account: read });
    queryClient.setQueryData(accountKey(accountId(3)), account(3));

    const refetching = queryClient
      .fetchQuery({ ...accountQuery(fake, accountId(3)), staleTime: 0 })
      .catch(() => undefined);
    watch.latest().push(accountFrame(3, { name: "Held" }));
    queryClient.clear();
    const fresh = queryClient.fetchQuery(accountQuery(fake, accountId(3)));
    answers[1]?.resolve(account(3, { name: "New session" }));
    await fresh;
    await settle();
    answers[0]?.resolve(account(3));
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

describe("the bots' and modes' queries", () => {
  /** The bots every page of `data` holds. */
  function bots(data: unknown) {
    return (data as InfiniteData<BotPage> | undefined)?.pages.flatMap((page) => page.items) ?? [];
  }

  it("read the bots a page at a time, and a bot frame reaches their pages", async () => {
    const { watch, fake, queryClient } = setup({
      bots: async (before) =>
        before === undefined ? { items: [bot(3), bot(1)], next: botId(1) } : { items: [] },
    });

    await queryClient.fetchInfiniteQuery(botsQuery(fake));
    watch.latest().push(botFrame(bot(2)));
    watch.latest().push(botFrame(bot(3, { desired_state: "running" })));

    expect(argsOf(fake, "bots")).toEqual([[undefined, undefined]]);
    expect(bots(queryClient.getQueryData(BOTS_LIST_KEY))).toEqual([
      bot(3, { desired_state: "running" }),
      bot(2),
      bot(1),
    ]);
  });

  it("read one bot, which follows its frames until it's removed: a screen on it reads it again and sees its 404 (R98)", async () => {
    let gone = false;
    const { watch, fake, queryClient } = setup({
      bot: async () => {
        if (gone) {
          throw refusal(404, "not_found");
        }
        return bot(1);
      },
    });
    const observer = new QueryObserver(queryClient, botQuery(fake, botId(1)));
    undo.push(observer.subscribe(() => {}));
    await settle();

    watch.latest().push(botFrame(bot(1, { desired_state: "running" })));
    const followed = queryClient.getQueryData(botKey(botId(1)));
    gone = true;
    watch.latest().push(event({ type: "bot_removed", bot_id: botId(1) }));
    await settle();

    expect(argsOf(fake, "bot")).toEqual([[botId(1)], [botId(1)]]);
    expect(followed).toEqual(bot(1, { desired_state: "running" }));
    expect(queryClient.getQueryData(botKey(botId(1)))).toBeUndefined();
    expect((observer.getCurrentResult().error as ApiError | null)?.error.api?.status).toBe(404);
  });

  it.each([
    [
      "its own query",
      (queryClient: QueryClient, fake: FakeApiClient) =>
        queryClient.fetchQuery(botQuery(fake, botId(1))),
    ],
    [
      "the list",
      (queryClient: QueryClient, fake: FakeApiClient) =>
        queryClient.fetchInfiniteQuery(botsQuery(fake)),
    ],
  ])(
    "reset a removed bot's account, the bot cached in %s: its data goes, a screen on it reads it again",
    async (_where, cache) => {
      let gone = false;
      const { watch, fake, queryClient } = setup({
        bot: async () => bot(1),
        bots: async () => ({ items: [bot(1)] }),
        account: async () => {
          if (gone) {
            throw refusal(404, "not_found");
          }
          return account(1);
        },
      });
      await cache(queryClient, fake);
      const observer = new QueryObserver(queryClient, accountQuery(fake, accountId(1)));
      undo.push(observer.subscribe(() => {}));
      await settle();

      gone = true;
      watch.latest().push(event({ type: "bot_removed", bot_id: botId(1) }));
      await settle();

      expect(queryClient.getQueryData(accountKey(accountId(1)))).toBeUndefined();
      expect((observer.getCurrentResult().error as ApiError | null)?.error.api?.status).toBe(404);
    },
  );

  it("read every page of the modes as one list's pages, which a mode frame reaches", async () => {
    const { watch, fake, queryClient } = setup({
      modes: async (before) =>
        before === undefined
          ? { items: [mode(5), mode(4)], next: modeId(4) }
          : { items: [mode(2)] },
    });

    await queryClient.fetchQuery(modesQuery(fake));
    watch.latest().push(event({ type: "mode", mode_id: modeId(3), mode: mode(3) }));

    expect(argsOf(fake, "modes")).toEqual([
      [undefined, PAGE_MAX],
      [modeId(4), PAGE_MAX],
    ]);
    expect(queryClient.getQueryData(MODES_LIST_KEY)?.pages.flatMap((page) => page.items)).toEqual([
      mode(5),
      mode(4),
      mode(3),
      mode(2),
    ]);
  });

  it("fail the modes' read when one of its pages fails", async () => {
    const { fake, queryClient } = setup({
      modes: async (before) => {
        if (before === undefined) {
          return { items: [mode(5)], next: modeId(5) };
        }
        throw fakeApiError("unreachable");
      },
    });

    const read = await queryClient.fetchQuery(modesQuery(fake)).catch((error: unknown) => error);

    expect((read as ApiError).error.code).toBe("unreachable");
    expect(queryClient.getQueryData(MODES_LIST_KEY)).toBeUndefined();
  });
});

describe("a first load (R87)", () => {
  it("is read once more when a refetch lands during it", async () => {
    const { answers, account: read } = reads();
    const { watch, fake, queryClient } = setup({ account: read });
    shown(queryClient, accountQuery(fake, accountId(3)));
    await settle();

    watch.latest().push(refetch("ready"));
    answers[0]?.resolve(account(3, { name: "Read before the refetch" }));
    await settle();
    answers[1]?.resolve(account(3, { name: "Read after it" }));
    await settle();

    expect(asked(fake, "account")).toBe(2);
    expect(queryClient.getQueryData(accountKey(accountId(3)))).toEqual(
      account(3, { name: "Read after it" }),
    );
  });

  it("is read once more when an invalidation lands during it", async () => {
    const { answers, account: read } = reads();
    const { watch, fake, queryClient } = setup({ account: read });
    shown(queryClient, accountQuery(fake, accountId(3)));
    await settle();

    watch.latest().push(event({ type: "bot_removed", bot_id: botId(9) }));
    answers[0]?.resolve(account(3, { name: "Read before the removal" }));
    await settle();
    answers[1]?.resolve(account(3, { name: "Read after it" }));
    await settle();

    expect(asked(fake, "account")).toBe(2);
    expect(queryClient.getQueryData(accountKey(accountId(3)))).toEqual(
      account(3, { name: "Read after it" }),
    );
  });
});

describe("the hold", () => {
  it("carries a cancelled fetch's hold into the fetch that replaced it", async () => {
    const { answers, account: read } = reads();
    const { watch, fake, queryClient } = setup({ account: read });
    queryClient.setQueryData(accountKey(accountId(3)), account(3));

    const first = queryClient
      .fetchQuery({ ...accountQuery(fake, accountId(3)), staleTime: 0 })
      .catch(() => undefined);
    watch.latest().push(accountFrame(3, { name: "Held" }));
    const replacing = queryClient.refetchQueries({
      queryKey: accountKey(accountId(3)),
      exact: true,
    });
    answers[1]?.resolve(account(3, { name: "Read again" }));
    await replacing;
    await settle();
    answers[0]?.resolve(account(3, { name: "Cancelled" }));
    await first;
    await settle();

    expect(asked(fake, "account")).toBe(2);
    expect(queryClient.getQueryData(accountKey(accountId(3)))).toEqual(
      account(3, { name: "Held" }),
    );
  });

  it("replays held events in the order they came", async () => {
    const answer = deferred<ReturnType<typeof pages>>();
    const { watch, queryClient } = setup();
    queryClient.setQueryData(MODES_LIST_KEY, pages({ items: [mode(1)] }));

    const refetching = queryClient.fetchQuery({
      queryKey: MODES_LIST_KEY,
      queryFn: () => answer.promise,
      staleTime: 0,
    });
    watch.latest().push(event({ type: "mode", mode_id: modeId(2), mode: mode(2) }));
    watch.latest().push(event({ type: "mode_removed", mode_id: modeId(2) }));
    answer.resolve(pages({ items: [mode(1)] }));
    await refetching;
    await settle();

    expect(queryClient.getQueryData(MODES_LIST_KEY)).toEqual(pages({ items: [mode(1)] }));
  });

  it(`replays exactly ${HOLD_MAX} held events`, async () => {
    const answer = deferred<AccountResponse>();
    const { watch, fake, queryClient } = setup({ account: () => answer.promise });
    queryClient.setQueryData(accountKey(accountId(3)), account(3));

    const refetching = queryClient.fetchQuery({
      ...accountQuery(fake, accountId(3)),
      staleTime: 0,
    });
    for (let n = 0; n < HOLD_MAX; n += 1) {
      watch.latest().push(accountFrame(3, { name: `AfkBot${n}` }));
    }
    answer.resolve(account(3, { name: "Fetched" }));
    await refetching;
    await settle();

    const state = queryClient.getQueryState(accountKey(accountId(3)));
    expect(state?.data).toEqual(account(3, { name: `AfkBot${HOLD_MAX - 1}` }));
    expect(state?.isInvalidated).toBe(false);
  });
});

describe("a reverted cancellation (R89)", () => {
  it("applies its hold to the data it kept, and a later fetch replays nothing old", async () => {
    const { answers, account: read } = reads();
    const { watch, fake, queryClient } = setup({ account: read });
    queryClient.setQueryData(accountKey(accountId(3)), account(3));
    // Marked stale before the fetch: the revert keeps that mark, and so must the hold's data.
    await queryClient.invalidateQueries({
      queryKey: accountKey(accountId(3)),
      exact: true,
      refetchType: "none",
    });

    const cancelled = queryClient
      .fetchQuery({ ...accountQuery(fake, accountId(3)), staleTime: 0 })
      .catch(() => undefined);
    watch.latest().push(accountFrame(3, { name: "Held" }));
    await queryClient.cancelQueries({ queryKey: accountKey(accountId(3)), exact: true });
    await cancelled;
    await settle();
    const afterTheCancel = queryClient.getQueryState(accountKey(accountId(3)));
    const later = queryClient.fetchQuery({ ...accountQuery(fake, accountId(3)), staleTime: 0 });
    answers[1]?.resolve(account(3, { name: "Fresh" }));
    await later;
    await settle();

    expect(afterTheCancel?.data).toEqual(account(3, { name: "Held" }));
    expect(afterTheCancel?.fetchStatus).toBe("idle");
    expect(afterTheCancel?.isInvalidated).toBe(true);
    expect(queryClient.getQueryData(accountKey(accountId(3)))).toEqual(
      account(3, { name: "Fresh" }),
    );
  });

  it("applies the hold of a query that read its signal when its last screen goes", async () => {
    const { answers, account: read } = reads();
    const { watch, queryClient } = setup();
    queryClient.setQueryData(accountKey(accountId(3)), account(3));
    const observer = new QueryObserver(queryClient, {
      queryKey: accountKey(accountId(3)),
      queryFn: ({ signal }) => {
        signal.throwIfAborted();
        return read();
      },
      staleTime: 0,
    });
    const unsubscribe = observer.subscribe(() => {});
    await settle();

    watch.latest().push(accountFrame(3, { name: "Held" }));
    unsubscribe();
    await settle();
    answers[0]?.resolve(account(3, { name: "Read before the change" }));
    await settle();

    expect(queryClient.getQueryData(accountKey(accountId(3)))).toEqual(
      account(3, { name: "Held" }),
    );
  });
});

describe("a cache entry of another shape (R90)", () => {
  const shapes: unknown[] = [
    { nope: 1 },
    "text",
    [null],
    { pages: [null] },
    { pages: [{ items: [null] }] },
  ];

  it.each([
    ["the bots list", BOTS_LIST_KEY, botFrame(bot(1))],
    ["the accounts list", ACCOUNT_LIST_KEY, accountFrame(1, {})],
    ["the waiting list", AWAITING_KEY, accountFrame(1, { status: "pending" })],
    ["a bot's chat", chatKey(botId(1)), event({ type: "chat", bot_id: botId(1), line: line(5) })],
    ["the modes list", MODES_LIST_KEY, event({ type: "mode", mode_id: modeId(2), mode: mode(2) })],
    [
      "the modes list, at a removal",
      MODES_LIST_KEY,
      event({ type: "mode_removed", mode_id: modeId(2) }),
    ],
  ] as const)("leaves %s alone and throws nothing", (_, key, update) => {
    for (const shape of shapes) {
      const { watch, queryClient } = setup();
      queryClient.setQueryData(key, shape);

      expect(() => watch.latest().push(update)).not.toThrow();
      expect(queryClient.getQueryData(key)).toEqual(shape);
    }
  });
});

describe("the banner (S62, R88)", () => {
  /** A layer over `watched(fake)`, as the runtime builds it, signed in. */
  function watchedSetup(handlers: Partial<ApiClient>) {
    const stores = createAppStores();
    const client = watched(new FakeApiClient(handlers), stores, {
      sessionEnded: () => {},
      recovered: () => {},
    });
    const layer = createLiveLayer(client, createQueryClient(), stores);
    undo.push(layer.attach());
    stores.session.getState().signIn();
    return { stores, layer };
  }

  it.each([
    ["unreachable", "down"],
    ["timed_out", "down"],
    ["certificate", "certificate"],
  ] as const)("the current watch's update whose connect was %s raises it as %s", (code, status) => {
    const watch = fakeWatch();
    const { stores } = watchedSetup({ watchEvents: watch.handler });

    watch.latest().push(notOpened(code));

    expect(stores.connection.getState().status).toBe(status);
  });

  it("leaves it alone for an update whose connect was refused otherwise", () => {
    const watch = fakeWatch();
    const { stores } = watchedSetup({ watchEvents: watch.handler });

    watch.latest().push(notOpened("api"));

    expect(stores.connection.getState().status).toBe("ok");
  });

  it("a replaced watch's update whose connect failed raises nothing", () => {
    const watch = manualWatch();
    const { stores } = watchedSetup({ watchEvents: watch.handler });
    stores.live.getState().restart();

    watch.calls[0]?.push(notOpened("unreachable"));

    expect(watch.calls).toHaveLength(2);
    expect(stores.connection.getState().status).toBe("ok");
  });

  it("a replaced watch's refused certificate raises nothing; the current one's does", async () => {
    const watch = manualWatch();
    const { stores } = watchedSetup({ watchEvents: watch.handler });
    stores.live.getState().restart();

    watch.calls[0]?.fail(fakeApiError("certificate"));
    await settle();
    const afterTheOld = stores.connection.getState().status;
    watch.calls[1]?.fail(fakeApiError("certificate"));
    await settle();

    expect(afterTheOld).toBe("ok");
    expect(stores.connection.getState().status).toBe("certificate");
  });
});

describe("a watch that stops by itself (S64, R91)", () => {
  it("says so while signed in", async () => {
    const { watch, stores } = setup();

    watch.latest().end();
    await settle();

    expect(stores.live.getState().hint).toEqual({ kind: "stopped", error: null });
  });

  it("says nothing while a server save runs", async () => {
    const { watch, stores } = setup();
    stores.session.getState().beginLogout();

    watch.latest().end();
    await settle();

    expect(stores.live.getState().hint).toEqual({ kind: "none" });
  });

  it("a restart starts one new watch, which clears it", async () => {
    const { watch, stores } = setup();
    watch.latest().end();
    await settle();

    stores.live.getState().restart();

    expect(watch.calls).toHaveLength(2);
    expect(stores.live.getState().hint).toEqual({ kind: "none" });
  });
});

describe("the live record (S71)", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("records the current watch's frames, never a replaced one's", async () => {
    const { watch, stores } = setup();
    const first = watch.latest();
    stores.live.getState().restart();

    first.push(botFrame(bot(1)));
    watch.latest().push(botFrame(bot(2)));

    expect(Object.keys(stores.record.getState().record.bots)).toEqual([botId(2)]);
  });

  it("clears at sign-out and at a new generation", async () => {
    const { watch, stores } = setup();
    watch.latest().push(botFrame(bot(1)));
    stores.session.getState().signOut();
    const signedOut = stores.record.getState().record;
    watch.latest().push(botFrame(bot(2)));
    stores.session.getState().signIn();
    watch.latest().push(botFrame(bot(3)));
    stores.session.getState().signIn();

    expect(signedOut.bots).toEqual({});
    expect(stores.record.getState().record.bots).toEqual({});
  });

  it("marks a gap when a new watch of the same sign-in is ready after the last one ended", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    vi.setSystemTime(Date.parse("2026-10-26T14:00:00.000Z"));
    const { watch, stores } = setup();
    watch.latest().push(refetch("ready"));
    watch.latest().end();
    await settle();

    vi.setSystemTime(Date.parse("2026-10-26T14:00:10.000Z"));
    stores.live.getState().restart();
    watch.latest().push(refetch("ready"));

    expect(stores.record.getState().record.gaps).toEqual([
      { arrived: Date.parse("2026-10-26T14:00:10.000Z"), downMs: 10_000 },
    ]);
  });

  it("fills in what a bot read knows, and drops a bot whose read answers 404", async () => {
    let gone = false;
    const { fake, queryClient, stores } = setup({
      bots: async () => ({ items: [bot(1), bot(2)] }),
      bot: async () => {
        if (gone) {
          throw refusal(404, "not_found");
        }
        return bot(1);
      },
    });

    await queryClient.fetchInfiniteQuery(botsQuery(fake));
    const filled = Object.keys(stores.record.getState().record.bots);
    gone = true;
    await queryClient.fetchQuery(botQuery(fake, botId(1))).catch(() => undefined);

    expect(filled).toEqual([botId(1), botId(2)]);
    expect(Object.keys(stores.record.getState().record.bots)).toEqual([botId(2)]);
  });
});

describe("the loss counter and the connected flag (S70, R115)", () => {
  it("count a loss at a watch's start, at the current watch's end and at every refetch", async () => {
    const { watch, stores } = setup();
    const losses = () => stores.live.getState().losses;
    const started = losses();
    const replaced = watch.latest();
    stores.live.getState().restart();
    const restarted = losses();
    replaced.end();
    await settle();
    const afterOld = losses();

    watch.latest().push(refetch("ready"));
    watch.latest().push(refetch("resync"));
    const refetched = losses();
    watch.latest().end();
    await settle();

    expect([started, restarted, afterOld, refetched, losses()]).toEqual([1, 2, 2, 4, 5]);
  });

  it("is connected from a refetch until a disconnect, an end, a new start or a sign-out", async () => {
    const { watch, stores } = setup();
    const connected = () => stores.live.getState().connected;
    const seen: boolean[] = [connected()];

    watch.latest().push(refetch("ready"));
    seen.push(connected());
    watch.latest().push(disconnected(5));
    seen.push(connected());
    watch.latest().push(refetch("ready"));
    stores.live.getState().restart();
    seen.push(connected());
    watch.latest().push(refetch("ready"));
    watch.latest().end();
    await settle();
    seen.push(connected());
    stores.live.getState().restart();
    watch.latest().push(refetch("ready"));
    stores.session.getState().signOut();
    seen.push(connected());

    expect(seen).toEqual([false, true, false, false, false, false]);
  });

  it("never starts a watch by itself", () => {
    const { fake, stores } = setup();
    const before = asked(fake, "watchEvents");

    stores.live.getState().lost();
    stores.live.getState().setConnected(true);

    expect(asked(fake, "watchEvents")).toBe(before);
  });

  it("lets a user's line and its chat_sent reach the outcomes a screen holds, and none nobody holds", () => {
    const { watch, queryClient } = setup();
    shown(queryClient, chatOutcomesQuery(botId(1)));
    const request = "01900000-0000-7000-8000-0000000004a1";

    for (const n of [1, 2]) {
      watch.latest().push(event({ type: "chat", bot_id: botId(n), line: line(8), request }));
      watch.latest().push(event({ type: "chat_sent", bot_id: botId(n), request }));
    }

    expect(queryClient.getQueryData(chatOutcomesKey(botId(1)))).toEqual({
      [request]: { line: 8, final: { kind: "sent" } },
    });
    expect(queryClient.getQueryData(chatOutcomesKey(botId(2)))).toBeUndefined();
  });

  it("resets a bot's outcomes at bot_removed, which a screen then holds empty", async () => {
    const { watch, queryClient } = setup();
    shown(queryClient, chatOutcomesQuery(botId(1)));
    const request = "01900000-0000-7000-8000-0000000004a1";
    watch.latest().push(event({ type: "chat_sent", bot_id: botId(1), request }));

    watch.latest().push(event({ type: "bot_removed", bot_id: botId(1) }));
    await settle();

    expect(queryClient.getQueryData(chatOutcomesKey(botId(1)))).toEqual({});
  });
});
