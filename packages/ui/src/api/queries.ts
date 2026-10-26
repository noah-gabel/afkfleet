// The queries more than one screen shares (Plan.md P8.6): the user and the server profile;
// and the lists the server answers in pages, newest first, which a screen shows a page at a
// time with "Show more" (P8.7, P8.8). No query or mutation may carry a secret in its request
// or its answer (a password, a TOTP secret or QR code, recovery codes, an invite code):
// those calls are direct calls, so nothing secret ever sits in TanStack Query's caches
// (CLAUDE.md, ADR-0017). A Microsoft link's start answers its user code, so it's a direct
// call too; its status carries none and is a query (features/accounts).
import {
  infiniteQueryOptions,
  queryOptions,
  useInfiniteQuery,
  useQuery,
} from "@tanstack/react-query";

import type { AccountPage } from "../generated/AccountPage";
import type { AccountResponse } from "../generated/AccountResponse";
import type { GrantPage } from "../generated/GrantPage";
import type { InvitePage } from "../generated/InvitePage";
import type { SessionPage } from "../generated/SessionPage";
import type { UserPage } from "../generated/UserPage";
import type { ApiClient } from "./ApiClient";
import { useApiClient } from "./context";
import { PAGE_MAX } from "./limits";

/** How long `me` stays fresh: a role changed by an Admin shows within about a minute. */
export const ME_STALE_MS = 60_000;

/** The signed-in user. */
export function meQuery(api: ApiClient) {
  return queryOptions({ queryKey: ["me"], queryFn: () => api.me(), staleTime: ME_STALE_MS });
}

/** The server profile; only the app changes it, and invalidates it when it does. */
export function serverProfileQuery(api: ApiClient) {
  return queryOptions({
    queryKey: ["serverProfile"],
    queryFn: () => api.serverProfile(),
    staleTime: Number.POSITIVE_INFINITY,
  });
}

/** The signed-in user, through the app's client. */
export function useMe() {
  return useQuery(meQuery(useApiClient()));
}

/** The server profile, through the app's client. */
export function useServerProfile() {
  return useQuery(serverProfileQuery(useApiClient()));
}

/** The user's sessions: every page loaded so far. */
export const SESSIONS_KEY = ["sessions"] as const;

/** The user's live sessions, a page at a time. */
export function sessionsQuery(api: ApiClient) {
  return infiniteQueryOptions({
    queryKey: SESSIONS_KEY,
    queryFn: ({ pageParam }) => api.sessions(pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page: SessionPage) => page.next,
  });
}

/** The user's sessions, through the app's client. */
export function useSessions() {
  return useInfiniteQuery(sessionsQuery(useApiClient()));
}

/** The users (Admins and the Owner): every page loaded so far. */
export const USERS_KEY = ["users"] as const;

/** The users, a page at a time. */
export function usersQuery(api: ApiClient) {
  return infiniteQueryOptions({
    queryKey: USERS_KEY,
    queryFn: ({ pageParam }) => api.users(pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page: UserPage) => page.next,
  });
}

/** The users, through the app's client. */
export function useUsers() {
  return useInfiniteQuery(usersQuery(useApiClient()));
}

/** The invites, in every state, without their codes: every page loaded so far. */
export const INVITES_KEY = ["invites"] as const;

/** The invites, a page at a time. */
export function invitesQuery(api: ApiClient) {
  return infiniteQueryOptions({
    queryKey: INVITES_KEY,
    queryFn: ({ pageParam }) => api.invites(pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page: InvitePage) => page.next,
  });
}

/** The invites, through the app's client. */
export function useInvites() {
  return useInfiniteQuery(invitesQuery(useApiClient()));
}

/** Where every account query's key starts, so one invalidation reaches them all. */
export const ACCOUNTS_KEY = ["accounts"] as const;

/** The accounts the user may see, every page loaded so far. */
export const ACCOUNT_LIST_KEY = ["accounts", "list"] as const;

/** The accounts the user may see, a page at a time, newest first. */
export function accountsQuery(api: ApiClient) {
  return infiniteQueryOptions({
    queryKey: ACCOUNT_LIST_KEY,
    queryFn: ({ pageParam }) => api.accounts(pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page: AccountPage) => page.next,
  });
}

/** The accounts, through the app's client. */
export function useAccounts() {
  return useInfiniteQuery(accountsQuery(useApiClient()));
}

/** The links waiting for the user's approval: all of them, for the count. */
export const AWAITING_KEY = ["accounts", "awaiting"] as const;

/** Every page of the links waiting for the user's approval; one failed page fails it all. */
async function everyAwaiting(api: ApiClient): Promise<AccountResponse[]> {
  const items: AccountResponse[] = [];
  let before: string | undefined;
  do {
    const page = await api.accountsAwaitingApproval(before, PAGE_MAX);
    items.push(...page.items);
    before = page.next;
  } while (before !== undefined);
  return items;
}

/**
 * The links waiting for the user's approval (S5 in ADR-0018's group F): read only for
 * Admins and the Owner (`enabled`), again when a screen that shows them mounts, after the
 * user's own account writes (`ACCOUNTS_KEY`), and when the window regains focus; no timer.
 */
export function awaitingQuery(api: ApiClient, enabled: boolean) {
  return queryOptions({
    queryKey: AWAITING_KEY,
    queryFn: () => everyAwaiting(api),
    enabled,
    refetchOnWindowFocus: true,
  });
}

/** One account's key. */
export function accountKey(id: string) {
  return ["accounts", "one", id] as const;
}

/** One account, as the user sees it. */
export function accountQuery(api: ApiClient, id: string) {
  return queryOptions({ queryKey: accountKey(id), queryFn: () => api.account(id) });
}

/** One account, through the app's client. */
export function useAccount(id: string) {
  return useQuery(accountQuery(useApiClient(), id));
}

/** Who else has access to account `id`, a page at a time. */
/** The key of who else has access to account `id`. */
export function grantsKey(id: string) {
  return ["accounts", "grants", id] as const;
}

export function grantsQuery(api: ApiClient, id: string) {
  return infiniteQueryOptions({
    queryKey: grantsKey(id),
    queryFn: ({ pageParam }) => api.accountGrants(id, pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page: GrantPage) => page.next,
  });
}

/** Who else has access to account `id`, through the app's client. */
export function useGrants(id: string, enabled: boolean) {
  return useInfiniteQuery({ ...grantsQuery(useApiClient(), id), enabled });
}

// The bots', their chat's and the modes' keys (P11.8; ADR-0020, S60): the live events
// patch whatever these hold, and H adds the queries that read them.

/** The bots the user may see, every page loaded so far. */
export const BOTS_LIST_KEY = ["bots", "list"] as const;

/** One bot's key. */
export function botKey(id: string) {
  return ["bots", "one", id] as const;
}

/** The key of one bot's chat, every page loaded so far. */
export function chatKey(id: string) {
  return ["bots", "chat", id] as const;
}

/** The modes the user may see, every page loaded so far. */
export const MODES_LIST_KEY = ["modes", "list"] as const;
