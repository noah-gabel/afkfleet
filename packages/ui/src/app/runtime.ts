// One App's moving parts (Plan.md P8.6, ADR-0017), built together: the UI stores, the
// QueryClient, the API client (validated, then watched) and the router, whose context
// holds the other three, and the live events layer (live.ts). Building it has no side
// effects (no timer, no call), since React may build it twice in StrictMode; the router's
// first load starts the launch, and `AppRoot` attaches the live events layer.
import type { QueryClient } from "@tanstack/react-query";

import type { ApiClient } from "../api/ApiClient";
import { validated } from "../api/validated";
import { endSession } from "./leave";
import { createLiveLayer, type LiveLayer } from "./live";
import { createQueryClient } from "./queryClient";
import { type AppRouter, createAppRouter } from "./router";
import { type AppStores, createAppStores } from "./stores";
import { watched } from "./watched";

/** One App's state; see the module comment. */
export interface AppRuntime {
  readonly stores: AppStores;
  readonly queryClient: QueryClient;
  /** The platform's client, validated and watched: the only one the UI uses. */
  readonly apiClient: ApiClient;
  readonly router: AppRouter;
  /** The live events layer: `AppRoot` attaches it. */
  readonly live: LiveLayer;
}

/** The runtime of an App over the platform's `apiClient`. */
export function createAppRuntime(apiClient: ApiClient): AppRuntime {
  const stores = createAppStores();
  const queryClient = createQueryClient();
  const routerRef: { current: AppRouter | null } = { current: null };
  const liveRef: { current: LiveLayer | null } = { current: null };
  const client = watched(validated(apiClient), stores, {
    sessionEnded() {
      if (routerRef.current !== null) {
        void endSession(routerRef.current, queryClient);
      }
    },
    recovered() {
      void queryClient.refetchQueries({ type: "active" });
      void routerRef.current?.invalidate();
      liveRef.current?.recovered();
    },
  });
  const live = createLiveLayer(client, queryClient, stores);
  liveRef.current = live;
  const router = createAppRouter({ queryClient, apiClient: client, stores });
  routerRef.current = router;
  return { stores, queryClient, apiClient: client, router, live };
}
