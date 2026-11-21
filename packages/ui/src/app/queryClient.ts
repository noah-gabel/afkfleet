// The app's TanStack Query client (Plan.md P8.6, ADR-0017). Nothing retries by itself:
// fleet-client already bounds every request, an error should show at once, and the
// banner's own check brings things back (ConnectionBanner). Nothing refetches on focus or
// on reconnect either; the banner's recovery refetches the active queries instead. Nothing
// pauses while Windows reports offline (`networkMode: "always"`, ADR-0020's note for I):
// fleet-client's own unreachable answer, with the banner, decides.
// A bot's script record, which only the live events make, stays for the session even while no
// screen shows it: the session's end clears the cache (P15.26).
import { QueryClient } from "@tanstack/react-query";

/** A fresh client with the app's defaults. */
export function createQueryClient(): QueryClient {
  const client = new QueryClient({
    defaultOptions: {
      queries: {
        retry: false,
        refetchOnWindowFocus: false,
        refetchOnReconnect: false,
        networkMode: "always",
      },
      mutations: { retry: false, networkMode: "always" },
    },
  });
  client.setQueryDefaults(["bots", "script"], {
    gcTime: Number.POSITIVE_INFINITY,
    staleTime: Number.POSITIVE_INFINITY,
  });
  return client;
}
