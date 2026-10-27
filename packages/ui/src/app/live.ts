// The live events layer (Plan.md P11.8; ADR-0020, S59–S65, R73): it keeps one watch of the
// session's live events and patches TanStack Query's cache from it.
//
// - **One watch per sign-in** (S61). A watch starts at each sign-in (a new generation), when
//   the connection recovers after the watch ended with a refused certificate (R73), and when
//   a server profile save changed nothing (`stores.live.restart()`). A watch that stops
//   (`Ok`) starts nothing: a newer watch or a switch stopped it, and a restart there would
//   skip the socket's backoff.
// - **Only the current watch counts** (S62): the newest one of the current generation while
//   the session isn't signed out. An update or an end of any other changes nothing, the
//   banner included: the current watch's failed connect and refused certificate raise it
//   here, not in `watched()` (R88).
// - **A refetch drops every hold and reads everything again** (S60): `ready`, `resync` and an
//   unreadable frame each supersede the events before them.
// - **An event patches the cache** (livePatches.ts), **held per query** (S60): a change for a
//   query that's fetching waits until its fetch succeeds, then all of its held changes are
//   applied in order, at most `HOLD_MAX`; past that, the query is read once more instead. A
//   fetch that fails applies them to the data it keeps, without a read, keeping its error
//   (S63). A cancelled fetch's hold carries over to the fetch that replaced it; a cancel that
//   reverts the query applies its hold at once, as a failed fetch does (R89); and a removed
//   query's goes with it.
// - **A first load reads once more** (R87): a refetch or an invalidation that lands while a
//   query without data fetches joins that fetch, whose success would then show what was read
//   before it as fresh; so such a query is read once more after it.
// - **The Header's hint** (U30, `stores.live`): a reconnect that waits, noting a
//   `too_many_connections`; the watch's stop with an error that isn't the session's end, or
//   its stop by itself while signed in with no server save running (S64's broken channel,
//   R91); nothing otherwise. Every refetch, event, new watch and sign-out clears it.
// - **The live record** (S71): every update of the current watch, and its end, go to the
//   record the timeline and the notices show; a bots query's read fills in what it doesn't
//   know, a bot's 404 drops it, and each sign-in and sign-out clears it.
// - **The chat's loss count** (S70, R115, `stores.live`): moved at each start, the current
//   watch's end and every refetch, so a pending chat mark ends with what may have been
//   missed; and whether the current watch is live, from its refetch until it goes down.
// Building it has no side effects; `attach` starts following the session.
import type { Query, QueryCacheNotifyEvent, QueryClient, QueryKey } from "@tanstack/react-query";

import type { ApiClient } from "../api/ApiClient";
import { toApiError } from "../api/ApiError";
import { type LivePatch, patchesFor, READ_AGAIN } from "../api/livePatches";
import { BOTS_LIST_KEY, botKey } from "../api/queries";
import type { BotPage } from "../generated/BotPage";
import type { BotResponse } from "../generated/BotResponse";
import type { LiveDisconnect } from "../generated/LiveDisconnect";
import type { LiveUpdateMessage } from "../generated/LiveUpdateMessage";
import type { AppStores, LiveHint } from "./stores";
import { connectionFailed } from "./watched";

/** The most changes held for one query while it fetches; past it, it's read once more. */
export const HOLD_MAX = 1024;

/** The live events layer of one App; see the module comment. */
export interface LiveLayer {
  /** Follows the session until the answer is called: a watch per sign-in. */
  attach(): () => void;
  /** The connection recovered: a watch that a refused certificate ended starts again. */
  recovered(): void;
}

/** A change that writes over a query's data. */
type SetPatch = Extract<LivePatch, { kind: "set" }>;

/** What a change only the server can decide asks of its query: a read, a stale mark, or nothing. */
type ReadAgain = "read" | "stale" | "nothing";

/** The changes held for one query while it fetches. */
interface Hold {
  readonly patches: SetPatch[];
  overflowed: boolean;
}

/** The live events layer over the app's (validated, watched) client. */
export function createLiveLayer(
  apiClient: ApiClient,
  queryClient: QueryClient,
  stores: AppStores,
): LiveLayer {
  const cache = queryClient.getQueryCache();
  const holds = new Map<string, Hold>();
  /** The current watch: its number and generation. */
  let current: { readonly number: number; readonly generation: number } | null = null;
  let issued = 0;
  /** The generation whose sign-in started a watch. */
  let started = 0;
  /** The generation whose last watch ended with a refused certificate. */
  let certificateEnded: number | null = null;
  let restarts = stores.live.getState().restarts;

  /** Whether a watch may count: signed in, or a save or logout under way (S62). */
  function inSession(): boolean {
    return stores.session.getState().phase !== "signed_out";
  }

  /** Starts a new watch of the current generation, which stops the older. */
  function start(): void {
    const generation = stores.session.getState().generation;
    issued += 1;
    const number = issued;
    current = { number, generation };
    certificateEnded = null;
    holds.clear();
    hint({ kind: "none" });
    lost(false);
    apiClient
      .watchEvents((update) => {
        if (current?.number !== number) {
          return;
        }
        if (update.update === "disconnected" && update.disconnect.why === "not_opened") {
          connectionFailed(stores, update.disconnect.error.code);
        }
        apply(update);
      })
      .then(
        () => ended(number, null),
        (rejection: unknown) => ended(number, rejection),
      );
  }

  /**
   * What watch `number`'s end changes: nothing unless it's the current one. `rejection` is
   * `null` for a stop.
   */
  function ended(number: number, rejection: unknown): void {
    if (current?.number !== number) {
      return;
    }
    const generation = current.generation;
    current = null;
    holds.clear();
    lost(false);
    stores.record.getState().down(Date.now());
    if (rejection === null) {
      // A newer watch or a server save stops it too, but neither while signed in with
      // nothing under way: then its channel broke (S64, R91).
      const phase = stores.session.getState().phase;
      hint(phase === "signed_in" ? { kind: "stopped", error: null } : { kind: "none" });
      return;
    }
    const error = toApiError(rejection).error;
    if (error.code === "no_session") {
      // A session's end ends the screens too.
      hint({ kind: "none" });
      return;
    }
    if (error.code === "certificate") {
      certificateEnded = generation;
    }
    connectionFailed(stores, error.code);
    hint({ kind: "stopped", error });
  }

  /** What the current watch's `update` changes. */
  function apply(update: LiveUpdateMessage): void {
    stores.record.getState().observe(update, Date.now());
    switch (update.update) {
      case "refetch":
        hint({ kind: "none" });
        lost(true);
        holds.clear();
        for (const query of cache.getAll()) {
          readOnceMore(query);
        }
        void queryClient.invalidateQueries();
        return;
      case "event":
        hint({ kind: "none" });
        for (const patch of patchesFor(update.event, cachedBot)) {
          change(patch);
        }
        return;
      case "disconnected":
        stores.live.getState().setConnected(false);
        hint(waiting(update.disconnect, update.retry_in_secs));
        return;
      default:
        return;
    }
  }

  /**
   * Updates may have been missed: a start, the current watch's end or a refetch, after which
   * the watch is `connected` or not (S70, R115).
   */
  function lost(connected: boolean): void {
    const live = stores.live.getState();
    live.lost();
    live.setConnected(connected);
  }

  /** Bot `id` as its own query or the bots' list holds it. */
  function cachedBot(id: string): BotResponse | undefined {
    return (
      queryClient.getQueryData(botKey(id)) ??
      queryClient
        .getQueryData(BOTS_LIST_KEY)
        ?.pages.flatMap((page) => page.items)
        .find((bot) => bot.id === id)
    );
  }

  /** Shows `next` in the Header, unless it's already shown. */
  function hint(next: LiveHint): void {
    const shown = stores.live.getState().hint;
    if (shown.kind !== "none" || next.kind !== "none") {
      stores.live.getState().setHint(next);
    }
  }

  /** Applies `patch` now, or holds it while its query fetches. */
  function change(patch: LivePatch): void {
    switch (patch.kind) {
      case "invalidate":
        for (const query of cache.findAll({ queryKey: patch.queryKey })) {
          readOnceMore(query);
        }
        void queryClient.invalidateQueries({ queryKey: patch.queryKey });
        return;
      case "reset":
        drop(patch.queryKey);
        void queryClient.resetQueries({ queryKey: patch.queryKey, exact: true });
        return;
      case "set": {
        const query = cache.find({ queryKey: patch.queryKey, exact: true });
        if (query === undefined) {
          return;
        }
        if (query.state.fetchStatus === "fetching") {
          hold(query, patch);
        } else {
          write(query, [patch], query.state.status === "error" ? "nothing" : "read");
        }
        return;
      }
      default:
        return;
    }
  }

  /** Drops what's held for the query `queryKey`: it starts over. */
  function drop(queryKey: QueryKey): void {
    const query = cache.find({ queryKey, exact: true });
    if (query !== undefined) {
      holds.delete(query.queryHash);
    }
  }

  /**
   * Has `query` read once more after its fetch, when it's a first load: a refetch or an
   * invalidation now joins that fetch instead of starting one (R87).
   */
  function readOnceMore(query: Query): void {
    if (query.state.fetchStatus !== "fetching" || query.state.data !== undefined) {
      return;
    }
    const held = holds.get(query.queryHash);
    if (held === undefined) {
      holds.set(query.queryHash, { patches: [], overflowed: true });
    } else {
      held.patches.length = 0;
      held.overflowed = true;
    }
  }

  /** Holds `patch` until `query`'s fetch ends. */
  function hold(query: Query, patch: SetPatch): void {
    const held = holds.get(query.queryHash) ?? { patches: [], overflowed: false };
    holds.set(query.queryHash, held);
    if (held.overflowed) {
      return;
    }
    if (held.patches.length >= HOLD_MAX) {
      held.patches.length = 0;
      held.overflowed = true;
      return;
    }
    held.patches.push(patch);
  }

  /**
   * Writes `patches`, in order, over the data `query` holds; nothing when it holds none.
   * A query in error keeps its error and its stale mark (S63), and so does one written
   * `dataOnly` (R89's revert), whatever its status. An update only the server can decide
   * reads the query once more, marks it stale, or neither, as `readAgain` says.
   */
  function write(
    query: Query,
    patches: readonly SetPatch[],
    readAgain: ReadAgain,
    dataOnly = false,
  ): void {
    const before = query.state.data;
    if (before === undefined) {
      return;
    }
    let data: unknown = before;
    let wanted = false;
    for (const patch of patches) {
      const next = patch.update(data);
      if (next === READ_AGAIN) {
        wanted = true;
      } else {
        data = next;
      }
    }
    if (data !== before) {
      if (dataOnly || query.state.status === "error") {
        query.setState({ ...query.state, data });
      } else {
        queryClient.setQueryData(query.queryKey, data);
      }
    }
    if (wanted) {
      settle(query, readAgain);
    }
  }

  /** Reads `query` once more, marks it stale, or neither, as `how` says. */
  function settle(query: Query, how: ReadAgain): void {
    if (how === "read") {
      void queryClient.invalidateQueries({ queryKey: query.queryKey, exact: true });
    } else if (how === "stale") {
      void queryClient.invalidateQueries({
        queryKey: query.queryKey,
        exact: true,
        refetchType: "none",
      });
    }
  }

  /** What a query's fetch that ended changes for its hold. */
  function fetched(event: QueryCacheNotifyEvent): void {
    if (event.type === "removed") {
      holds.delete(event.query.queryHash);
      return;
    }
    if (event.type !== "updated") {
      return;
    }
    const held = holds.get(event.query.queryHash);
    const action = event.action;
    if (held === undefined) {
      return;
    }
    if (action.type === "success" && action.manual !== true) {
      holds.delete(event.query.queryHash);
      if (held.overflowed) {
        settle(event.query, "read");
      } else {
        write(event.query, held.patches, "read");
      }
    } else if (action.type === "error") {
      holds.delete(event.query.queryHash);
      write(event.query, held.patches, "nothing");
    } else if (action.type === "setState" && event.query.state.fetchStatus !== "fetching") {
      // A cancel that reverted the query (R89): no success or error follows, so its held
      // changes apply now to the data it kept, without a read, as a failed fetch's do.
      holds.delete(event.query.queryHash);
      if (held.overflowed) {
        settle(event.query, "stale");
      } else {
        write(event.query, held.patches, "stale", true);
      }
    }
  }

  /**
   * What a bots query's read tells the live record (S71): the bots it read, where the record
   * doesn't know them yet; and that a bot whose read answered 404 is gone.
   */
  function recorded(event: QueryCacheNotifyEvent): void {
    if (event.type !== "updated") {
      return;
    }
    const [root, which, id] = event.query.queryKey;
    if (root !== "bots" || (which !== "list" && which !== "one")) {
      return;
    }
    const record = stores.record.getState();
    if (event.action.type === "success") {
      const data: unknown = event.query.state.data;
      if (which === "one" && isBot(data)) {
        record.known([data]);
      } else if (which === "list" && isBotPages(data)) {
        record.known(data.pages.flatMap((page) => page.items));
      }
    } else if (
      event.action.type === "error" &&
      which === "one" &&
      typeof id === "string" &&
      toApiError(event.action.error).error.api?.status === 404
    ) {
      record.drop(id);
    }
  }

  /** Follows the session's phase and the restarts asked for. */
  function followed(): void {
    const session = stores.session.getState();
    if (!inSession()) {
      current = null;
      holds.clear();
      stores.live.getState().setConnected(false);
      hint({ kind: "none" });
      stores.record.getState().clear();
      return;
    }
    if (session.phase === "signed_in" && session.generation !== started) {
      started = session.generation;
      stores.record.getState().clear();
      start();
      return;
    }
    const asked = stores.live.getState().restarts;
    if (asked !== restarts) {
      restarts = asked;
      if (session.phase === "signed_in") {
        start();
      }
    }
  }

  return {
    attach() {
      restarts = stores.live.getState().restarts;
      const unsubscribes = [
        cache.subscribe(fetched),
        cache.subscribe(recorded),
        stores.session.subscribe(followed),
        stores.live.subscribe(followed),
      ];
      followed();
      return () => {
        for (const unsubscribe of unsubscribes) {
          unsubscribe();
        }
      };
    },
    recovered() {
      const session = stores.session.getState();
      if (
        current === null &&
        session.phase === "signed_in" &&
        certificateEnded === session.generation
      ) {
        start();
      }
    },
  };
}

/** Whether `data` is a bot, as a bot's query holds it. */
function isBot(data: unknown): data is BotResponse {
  return typeof data === "object" && data !== null && "status" in data && "account" in data;
}

/** Whether `data` holds bots' pages, as the bots' list holds them. */
function isBotPages(data: unknown): data is { readonly pages: readonly BotPage[] } {
  if (typeof data !== "object" || data === null || !("pages" in data)) {
    return false;
  }
  const { pages } = data;
  return (
    Array.isArray(pages) &&
    pages.every(
      (page: unknown) =>
        typeof page === "object" &&
        page !== null &&
        "items" in page &&
        Array.isArray(page.items) &&
        page.items.every(isBot),
    )
  );
}

/**
 * The hint for a socket that went down for `disconnect` and waits `secs` (U30): nothing for
 * no wait, the routine reconnect an access token's end asks for.
 */
function waiting(disconnect: LiveDisconnect, secs: number): LiveHint {
  if (secs <= 0) {
    return { kind: "none" };
  }
  const tooMany =
    disconnect.why === "not_opened" && disconnect.error.api?.code === "too_many_connections";
  return { kind: "waiting", secs, until: Date.now() + secs * 1000, tooMany };
}
