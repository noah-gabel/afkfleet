// The cache changes a live event asks for (Plan.md P11.8; ADR-0020, S60, S65): pure, so the
// live layer (app/live.ts) decides when each is written, and holds it while its query
// fetches. A `bot` frame is the whole bot as the viewer's `GET /bots/{id}` shows it, with its
// account as `GET /accounts/{id}` shows it, so a patch replaces what's cached; an item not
// cached yet is placed by the keyset paging rule (newest first by ID, never below a loaded
// page that has more after it). No patch creates a query's entry: the layer writes only over
// data a query holds.
import type { InfiniteData, QueryKey } from "@tanstack/react-query";

import type { AccountResponse } from "../generated/AccountResponse";
import type { BotResponse } from "../generated/BotResponse";
import type { EventMessage } from "../generated/EventMessage";
import {
  ACCOUNT_LIST_KEY,
  ACCOUNTS_KEY,
  AWAITING_KEY,
  accountKey,
  BOTS_LIST_KEY,
  botKey,
  chatKey,
  grantsKey,
  MODES_LIST_KEY,
} from "./queries";

/** What an update answers when only the server can decide: read the query again. */
export const READ_AGAIN = Symbol("read again");

/** A query's new data from its data: the same data for no change, or `READ_AGAIN`. */
export type LiveUpdater = (data: unknown) => unknown;

/** One change a live event asks of the cache. */
export type LivePatch =
  /** Writes over the data the query holds, if it holds any. */
  | { readonly kind: "set"; readonly queryKey: QueryKey; readonly update: LiveUpdater }
  /** Reads every query under the key again. */
  | { readonly kind: "invalidate"; readonly queryKey: QueryKey }
  /** Drops the query. */
  | { readonly kind: "remove"; readonly queryKey: QueryKey }
  /** Resets the query: empty, read again when shown. */
  | { readonly kind: "reset"; readonly queryKey: QueryKey };

/** One page of a list the server answers newest first. */
interface Page<T> {
  readonly items: readonly T[];
  readonly next?: unknown;
}

/** An item of such a list: its ID orders it, a UUID v7's text or a row's number. */
interface Listed {
  readonly id: string | number;
}

/** The changes `event` asks for; none for a kind that changes no cache. */
export function patchesFor(event: EventMessage): LivePatch[] {
  switch (event.type) {
    case "bot":
      return event.bot === undefined ? [] : botPatches(event.bot);
    case "bot_removed":
      return event.bot_id === undefined ? [] : removedPatches(event.bot_id);
    case "chat": {
      const line = event.line;
      return event.bot_id === undefined || line === undefined
        ? []
        : [set(chatKey(event.bot_id), (data) => placed(pages(data), line))];
    }
    case "mode": {
      const mode = event.mode;
      return mode === undefined ? [] : [set(MODES_LIST_KEY, (data) => placed(pages(data), mode))];
    }
    case "mode_removed": {
      const id = event.mode_id;
      return id === undefined ? [] : [set(MODES_LIST_KEY, (data) => without(pages(data), id))];
    }
    case "ready":
    case "resync":
    case "bot_died":
    case "bot_alert":
    case "chat_sent":
    case "chat_failed":
      // A refetch comes as its own update; the rest are notices, which patch nothing.
      return [];
    default:
      // A kind this version doesn't know.
      return [];
  }
}

/** A `bot` frame's changes: the bot's caches, then its account's. */
function botPatches(bot: BotResponse): LivePatch[] {
  const account = bot.account;
  return [
    set(BOTS_LIST_KEY, (data) => placed(pages(data), bot)),
    set(botKey(bot.id), () => bot),
    set(ACCOUNT_LIST_KEY, (data) => placed(pages(data), account)),
    set(accountKey(account.id), () => account),
    { kind: "invalidate", queryKey: grantsKey(account.id) },
    set(AWAITING_KEY, (data) => awaiting(data as readonly AccountResponse[], account)),
  ];
}

/** A `bot_removed`'s changes: no bot→account mapping here, so every account is read again. */
function removedPatches(id: string): LivePatch[] {
  return [
    set(BOTS_LIST_KEY, (data) => without(pages(data), id)),
    { kind: "remove", queryKey: botKey(id) },
    { kind: "reset", queryKey: chatKey(id) },
    { kind: "invalidate", queryKey: ACCOUNTS_KEY },
  ];
}

/**
 * The links waiting for the viewer's approval after `account`'s frame (S65): only the
 * server knows which pending links it lists to whom, so a pending one it doesn't list yet,
 * or a status this version doesn't know, reads the list again.
 */
function awaiting(
  list: readonly AccountResponse[],
  account: AccountResponse,
): readonly AccountResponse[] | typeof READ_AGAIN {
  const listed = list.some((one) => one.id === account.id);
  switch (account.status) {
    case "pending":
      return listed ? list.map((one) => (one.id === account.id ? account : one)) : READ_AGAIN;
    case "active":
    case "reauth_required":
    case "refused":
      return listed ? list.filter((one) => one.id !== account.id) : list;
    default:
      return READ_AGAIN;
  }
}

/** A patch that writes over `queryKey`'s data. */
function set(queryKey: QueryKey, update: LiveUpdater): LivePatch {
  return { kind: "set", queryKey, update };
}

/** `data` as the pages of a list. */
function pages(data: unknown): InfiniteData<Page<Listed>> {
  return data as InfiniteData<Page<Listed>>;
}

/**
 * `data` with `item` in it: in its place when cached, otherwise where the keyset paging rule
 * puts it, newest first; unchanged when it belongs to a page not loaded yet.
 */
export function placed<T extends Listed>(
  data: InfiniteData<Page<T>>,
  item: T,
): InfiniteData<Page<T>> {
  const at = data.pages.findIndex((page) => page.items.some((one) => one.id === item.id));
  if (at >= 0) {
    return withPage(data, at, (items) => items.map((one) => (one.id === item.id ? item : one)));
  }
  const into = data.pages.findIndex((page, index) => {
    const oldest = page.items.at(-1);
    const reachesTheEnd = index === data.pages.length - 1 && page.next === undefined;
    return reachesTheEnd || (oldest !== undefined && newer(item.id, oldest.id));
  });
  if (into < 0) {
    return data;
  }
  return withPage(data, into, (items) => {
    const before = items.findIndex((one) => newer(item.id, one.id));
    return before < 0
      ? [...items, item]
      : [...items.slice(0, before), item, ...items.slice(before)];
  });
}

/** `data` without the item `id`; unchanged when it holds none. */
export function without<T extends Listed>(
  data: InfiniteData<Page<T>>,
  id: T["id"],
): InfiniteData<Page<T>> {
  const at = data.pages.findIndex((page) => page.items.some((one) => one.id === id));
  return at < 0 ? data : withPage(data, at, (items) => items.filter((one) => one.id !== id));
}

/** Whether ID `a` is newer than `b`: a larger row number, or a later UUID v7, whose text
 * sorts by its time. */
function newer(a: string | number, b: string | number): boolean {
  return typeof a === "number" && typeof b === "number" ? a > b : String(a) > String(b);
}

/** `data` with page `at`'s items changed by `change`. */
function withPage<T>(
  data: InfiniteData<Page<T>>,
  at: number,
  change: (items: readonly T[]) => readonly T[],
): InfiniteData<Page<T>> {
  return {
    ...data,
    pages: data.pages.map((page, index) =>
      index === at ? { ...page, items: change(page.items) } : page,
    ),
  };
}
