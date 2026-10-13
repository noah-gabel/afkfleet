import { useMemo } from "react";

import type { ApiClient } from "./api/ApiClient";
import { ApiClientProvider } from "./api/context";
import { validated } from "./api/validated";

/**
 * The app's root component. It takes the platform's API client (the desktop app's
 * `TauriApiClient`, a test's `FakeApiClient`) and provides it, behind the zod checks of
 * every input, to everything below. For now it only names the app; the routes arrive with
 * P8.6.
 */
export function App({ apiClient }: { apiClient: ApiClient }) {
  const client = useMemo(() => validated(apiClient), [apiClient]);
  return (
    <ApiClientProvider client={client}>
      <main className="flex min-h-screen flex-col items-center justify-center gap-2 bg-background p-8 text-foreground">
        <h1 className="text-3xl font-semibold tracking-tight">afkfleet</h1>
        <p className="text-muted-foreground">The desktop app is being set up.</p>
      </main>
    </ApiClientProvider>
  );
}
