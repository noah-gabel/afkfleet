// The live events layer (Plan.md P11.8; ADR-0020, S59–S65, R73): it keeps one watch of the
// session's live events and patches TanStack Query's cache from it.
//
// - **One watch per sign-in** (S61). A watch starts at each sign-in (a new generation), when
//   the connection recovers after the watch ended with a refused certificate (R73), and when
//   a server profile save changed nothing (`stores.live.restart()`). A watch that stops
//   (`Ok`) starts nothing: a newer watch or a switch stopped it, and a restart there would
//   skip the socket's backoff.
// - **Only the current watch counts** (S62): the newest one of the current generation while
//   the session isn't signed out. An update or an end of any other changes nothing.
// - **A refetch drops every hold and reads everything again** (S60): `ready`, `resync` and an
//   unreadable frame each supersede the events before them.
// - **An event patches the cache** (livePatches.ts), **held per query** (S60): a change for a
//   query that's fetching waits until its fetch succeeds, then all of its held changes are
//   applied in order, at most `HOLD_MAX`; past that, the query is read once more instead. A
//   fetch that fails applies them to the data it keeps, without a read, keeping its error
//   (S63). A cancelled fetch's hold carries over to the fetch that replaced it, and a
//   removed query's goes with it.
// Building it has no side effects; `attach` starts following the session.
import type { Query, QueryCacheNotifyEvent, QueryClient, QueryKey } from "@tanstack/react-query";

import type { ApiClient } from "../api/ApiClient";
import { ApiError } from "../api/ApiError";
import { type LivePatch, patchesFor, READ_AGAIN } from "../api/livePatches";
import type { LiveUpdateMessage } from "../generated/LiveUpdateMessage";
import type { AppStores } from "./stores";

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
    apiClient
      .watchEvents((update) => {
        if (current?.number === number) {
          apply(update);
        }
      })
      .then(
        () => ended(number, null),
        (rejection: unknown) => ended(number, rejection),
      );
  }

  /** What watch `number`'s end changes: nothing unless it's the current one. */
  function ended(number: number, rejection: unknown): void {
    if (current?.number !== number) {
      return;
    }
    const generation = current.generation;
    current = null;
    holds.clear();
    if (rejection instanceof ApiError && rejection.error.code === "certificate") {
      certificateEnded = generation;
    }
  }

  /** What the current watch's `update` changes. */
  function apply(update: LiveUpdateMessage): void {
    switch (update.update) {
      case "refetch":
        holds.clear();
        void queryClient.invalidateQueries();
        return;
      case "event":
        for (const patch of patchesFor(update.event)) {
          change(patch);
        }
        return;
      case "disconnected":
        return;
      default:
        return;
    }
  }

  /** Applies `patch` now, or holds it while its query fetches. */
  function change(patch: LivePatch): void {
    switch (patch.kind) {
      case "invalidate":
        void queryClient.invalidateQueries({ queryKey: patch.queryKey });
        return;
      case "remove":
        drop(patch.queryKey);
        queryClient.removeQueries({ queryKey: patch.queryKey, exact: true });
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
          write(query, [patch], query.state.status !== "error");
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
   * A query in error keeps its error and its stale mark (S63). An update only the server can
   * decide reads the query once more, if `mayRead`.
   */
  function write(query: Query, patches: readonly SetPatch[], mayRead: boolean): void {
    const before = query.state.data;
    if (before === undefined) {
      return;
    }
    let data: unknown = before;
    let readAgain = false;
    for (const patch of patches) {
      const next = patch.update(data);
      if (next === READ_AGAIN) {
        readAgain = true;
      } else {
        data = next;
      }
    }
    if (data !== before) {
      if (query.state.status === "error") {
        query.setState({ ...query.state, data });
      } else {
        queryClient.setQueryData(query.queryKey, data);
      }
    }
    if (readAgain && mayRead) {
      void queryClient.invalidateQueries({ queryKey: query.queryKey, exact: true });
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
        void queryClient.invalidateQueries({ queryKey: event.query.queryKey, exact: true });
      } else {
        write(event.query, held.patches, true);
      }
    } else if (action.type === "error") {
      holds.delete(event.query.queryHash);
      write(event.query, held.patches, false);
    }
  }

  /** Follows the session's phase and the restarts asked for. */
  function followed(): void {
    const session = stores.session.getState();
    if (!inSession()) {
      current = null;
      holds.clear();
      return;
    }
    if (session.phase === "signed_in" && session.generation !== started) {
      started = session.generation;
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
