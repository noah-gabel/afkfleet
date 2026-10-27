// The reads that take every page at once (ADR-0020, S68, R99): a server whose pages don't
// move on fails the read instead of looping, and a cancelled read stops between pages.
import { type QueryClient, QueryObserver } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";

import { createQueryClient } from "../app/queryClient";
import { accountId } from "../test/accounts";
import { deferred } from "../test/fixtures";
import { botId, modeId } from "../test/live";
import type { ApiClient } from "./ApiClient";
import { ApiError } from "./ApiError";
import { FakeApiClient } from "./FakeApiClient";
import {
  AWAITING_KEY,
  awaitingQuery,
  chatOutcomesKey,
  chatOutcomesQuery,
  chatQuery,
  MODES_LIST_KEY,
  modesQuery,
  PAGES_DONT_MOVE_ON,
  recordAnswer,
} from "./queries";

/** An empty page, with `next` when more come. */
interface Page {
  readonly items: never[];
  readonly next?: string;
}

/** A list read every page at a time: its method, its query and its IDs. */
interface EveryPage {
  readonly name: string;
  readonly method: "modes" | "accountsAwaitingApproval";
  read(queryClient: QueryClient, fake: FakeApiClient): Promise<unknown>;
  cancel(queryClient: QueryClient): Promise<void>;
  readonly id: (n: number) => string;
}

const LISTS: readonly EveryPage[] = [
  {
    name: "the modes",
    method: "modes",
    read: (queryClient, fake) => queryClient.fetchQuery(modesQuery(fake)),
    cancel: (queryClient) => queryClient.cancelQueries({ queryKey: MODES_LIST_KEY }),
    id: modeId,
  },
  {
    name: "the links waiting for approval",
    method: "accountsAwaitingApproval",
    read: (queryClient, fake) => queryClient.fetchQuery(awaitingQuery(fake, true)),
    cancel: (queryClient) => queryClient.cancelQueries({ queryKey: AWAITING_KEY }),
    id: accountId,
  },
];

/** The most pages a test's server answers before it gives up, so a loop can't run on. */
const PAGES_MAX = 5;

/** A server of `list` whose page after `before` is `answer(before)`, at most `PAGES_MAX` times. */
function server(list: EveryPage, answer: (before: string | undefined) => Promise<Page>) {
  let pages = 0;
  const handler = async (before?: string) => {
    pages += 1;
    if (pages > PAGES_MAX) {
      throw new Error("asked for too many pages");
    }
    return answer(before);
  };
  const handlers: Partial<ApiClient> = { [list.method]: handler };
  return new FakeApiClient(handlers);
}

/** The cursors `fake` was asked `list`'s pages with. */
function cursors(fake: FakeApiClient, list: EveryPage): unknown[] {
  return fake.calls.filter((call) => call.method === list.method).map((call) => call.args[0]);
}

/** Lets every pending promise and timer settle. */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

/** Whether `error` is the read's failure for pages that don't move on. */
function didntMoveOn(error: unknown): boolean {
  return (
    error instanceof ApiError &&
    error.error.code === "unexpected_response" &&
    error.error.message === PAGES_DONT_MOVE_ON
  );
}

describe.each(LISTS)("reading every page of $name", (list) => {
  it("fails when a page's next is the cursor it was asked with", async () => {
    const fake = server(list, async (before) => ({ items: [], next: before ?? list.id(5) }));

    const read = await list.read(createQueryClient(), fake).catch((error: unknown) => error);

    expect(didntMoveOn(read)).toBe(true);
    expect(cursors(fake, list)).toEqual([undefined, list.id(5)]);
  });

  it("fails when a page's next goes back to a cursor asked before", async () => {
    const next = new Map([
      [undefined, list.id(5)],
      [list.id(5), list.id(3)],
      [list.id(3), list.id(5)],
    ]);
    const fake = server(list, async (before) => ({ items: [], next: next.get(before) ?? "" }));

    const read = await list.read(createQueryClient(), fake).catch((error: unknown) => error);

    expect(didntMoveOn(read)).toBe(true);
    expect(cursors(fake, list)).toEqual([undefined, list.id(5), list.id(3)]);
  });

  it("reads pages whose next moves on to their end", async () => {
    const fake = server(list, async (before) => ({
      items: [],
      ...(before === undefined ? { next: list.id(5) } : {}),
    }));

    await list.read(createQueryClient(), fake);

    expect(cursors(fake, list)).toEqual([undefined, list.id(5)]);
  });

  it("asks for no further page once the read is cancelled", async () => {
    const first = deferred<Page>();
    const fake = server(list, (before) =>
      before === undefined ? first.promise : Promise.resolve({ items: [] }),
    );
    const queryClient = createQueryClient();

    const reading = list.read(queryClient, fake).catch(() => undefined);
    await settle();
    await list.cancel(queryClient);
    first.resolve({ items: [], next: list.id(5) });
    await reading;
    await settle();

    expect(cursors(fake, list)).toEqual([undefined]);
  });
});

describe("the chat's pages (R120)", () => {
  it("fail an older page whose next doesn't move past the cursor it was asked with", async () => {
    const fake = new FakeApiClient({
      botChat: async (_id, before) => ({ items: [], next: before ?? 5 }),
    });

    const read = await createQueryClient()
      .fetchInfiniteQuery({ ...chatQuery(fake, botId(1)), pages: 2 })
      .catch((error: unknown) => error);

    expect(didntMoveOn(read)).toBe(true);
  });

  it("read older pages whose next moves on, to their end", async () => {
    const fake = new FakeApiClient({
      botChat: async (_id, before) =>
        before === undefined ? { items: [], next: 5 } : { items: [] },
    });

    const read = await createQueryClient().fetchInfiniteQuery({
      ...chatQuery(fake, botId(1)),
      pages: 2,
    });

    expect(read.pageParams).toEqual([undefined, 5]);
  });
});

describe("the chat's outcomes (S70, R121)", () => {
  const REQUEST = "01900000-0000-7000-8000-0000000004a1";
  const ANSWER = { request_id: REQUEST, message_id: 7, outcome: "pending" } as const;

  /** The outcomes of bot 1 held by a screen, as the console holds them. */
  function held(queryClient: QueryClient) {
    const observer = new QueryObserver(queryClient, chatOutcomesQuery(botId(1)));
    return observer.subscribe(() => {});
  }

  it("hold nothing at first, and a send's answer links its line, pending from its start", () => {
    const queryClient = createQueryClient();
    const stop = held(queryClient);

    const before = queryClient.getQueryData(chatOutcomesKey(botId(1)));
    recordAnswer(queryClient, botId(1), ANSWER, 2);

    expect(before).toEqual({});
    expect(queryClient.getQueryData(chatOutcomesKey(botId(1)))).toEqual({
      [REQUEST]: { line: 7, pendingSince: 2 },
    });
    stop();
  });

  it("keep their data when everything is read again, and are never fetched", async () => {
    const queryClient = createQueryClient();
    const stop = held(queryClient);
    recordAnswer(queryClient, botId(1), ANSWER, 2);

    await queryClient.invalidateQueries();

    expect(queryClient.getQueryData(chatOutcomesKey(botId(1)))).toEqual({
      [REQUEST]: { line: 7, pendingSince: 2 },
    });
    expect(queryClient.isFetching()).toBe(0);
    stop();
  });

  it("outlive their screen past the cache's usual 5 minutes (R121)", () => {
    vi.useFakeTimers();
    try {
      const queryClient = createQueryClient();
      const stop = held(queryClient);
      recordAnswer(queryClient, botId(1), ANSWER, 2);

      stop();
      vi.advanceTimersByTime(5 * 60_000 + 1);

      expect(queryClient.getQueryData(chatOutcomesKey(botId(1)))).toEqual({
        [REQUEST]: { line: 7, pendingSince: 2 },
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("build nothing for an answer after the session's end emptied the cache", () => {
    const queryClient = createQueryClient();
    const stop = held(queryClient);
    queryClient.clear();

    recordAnswer(queryClient, botId(1), ANSWER, 2);

    expect(queryClient.getQueryData(chatOutcomesKey(botId(1)))).toBeUndefined();
    stop();
  });
});
