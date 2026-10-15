import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { useState } from "react";

import type { ApiClient } from "./api/ApiClient";
import { ApiClientProvider } from "./api/context";
import { StoresProvider } from "./app/context";
import { useWindowFocus } from "./app/focus";
import { type AppRuntime, createAppRuntime } from "./app/runtime";
import { Toaster } from "./components/Toaster";
import { AppErrorBoundary } from "./features/shell/components/AppErrorBoundary";

/**
 * The app's root component. It takes the platform's API client (the desktop app's
 * `TauriApiClient`, a test's `FakeApiClient`) and builds the app around it once: the zod
 * checks of every input, the watch over every call's outcome, the query cache, the UI
 * stores and the router (app/runtime.ts). A later `apiClient` is ignored.
 */
export function App({ apiClient }: { apiClient: ApiClient }) {
  const [runtime] = useState(() => createAppRuntime(apiClient));
  return <AppRoot runtime={runtime} />;
}

/**
 * The app over an existing runtime: the error boundary around everything, the providers,
 * the router and the toasts. Tests render it to reach the runtime; `onRestart` replaces
 * the error screen's reload. While it's mounted, the window's focus counts (app/focus.ts).
 */
export function AppRoot({ runtime, onRestart }: { runtime: AppRuntime; onRestart?: () => void }) {
  useWindowFocus();
  return (
    <AppErrorBoundary {...(onRestart === undefined ? {} : { onRestart })}>
      <ApiClientProvider client={runtime.apiClient}>
        <QueryClientProvider client={runtime.queryClient}>
          <StoresProvider stores={runtime.stores}>
            <RouterProvider router={runtime.router} />
            <Toaster />
          </StoresProvider>
        </QueryClientProvider>
      </ApiClientProvider>
    </AppErrorBoundary>
  );
}
