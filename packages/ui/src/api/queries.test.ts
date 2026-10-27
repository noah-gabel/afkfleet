// The reads that take every page at once (ADR-0020, S68, R99): a server whose pages don't
// move on fails the read instead of looping, and a cancelled read stops between pages.
import type { QueryClient } from "@tanstack/react-query";
import { describe, expect, it } from "vitest";

import { createQueryClient } from "../app/queryClient";
import { accountId } from "../test/accounts";
import { deferred } from "../test/fixtures";
import { modeId } from "../test/live";
import type { ApiClient } from "./ApiClient";
import { ApiError } from "./ApiError";
import { FakeApiClient } from "./FakeApiClient";
import {
  AWAITING_KEY,
  awaitingQuery,
  MODES_LIST_KEY,
  modesQuery,
  PAGES_DONT_MOVE_ON,
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
