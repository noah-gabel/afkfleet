// The queries more than one screen shares (Plan.md P8.6): the user and the server profile;
// and the lists the server answers in pages, newest first, which a screen shows a page at a
// time with "Show more" (P8.7, P8.8). No query or mutation may carry a secret in its request
// or its answer (a password, a TOTP secret or QR code, recovery codes, an invite code):
// those calls are direct calls, so nothing secret ever sits in TanStack Query's caches
// (CLAUDE.md, ADR-0017).
import {
  infiniteQueryOptions,
  queryOptions,
  useInfiniteQuery,
  useQuery,
} from "@tanstack/react-query";

import type { InvitePage } from "../generated/InvitePage";
import type { SessionPage } from "../generated/SessionPage";
import type { UserPage } from "../generated/UserPage";
import type { ApiClient } from "./ApiClient";
import { useApiClient } from "./context";

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
