// The app's TanStack Query client (Plan.md P8.6, ADR-0017). Nothing retries by itself:
// fleet-client already bounds every request, an error should show at once, and the
// banner's own check brings things back (ConnectionBanner). Nothing refetches on focus or
// on reconnect either; the banner's recovery refetches the active queries instead.
import { QueryClient } from "@tanstack/react-query";

/** A fresh client with the app's defaults. */
export function createQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, refetchOnWindowFocus: false, refetchOnReconnect: false },
      mutations: { retry: false },
    },
  });
}
