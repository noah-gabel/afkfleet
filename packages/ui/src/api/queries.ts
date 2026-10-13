// The queries more than one screen shares (Plan.md P8.6): the user and the server profile.
// No query or mutation may carry a secret in its request or its answer (a password, a TOTP
// secret or QR code, recovery codes, an invite code): those calls are direct calls, so
// nothing secret ever sits in TanStack Query's caches (CLAUDE.md, ADR-0017).
import { queryOptions, useQuery } from "@tanstack/react-query";

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
