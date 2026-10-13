// The router's root (Plan.md P8.6): the banner above every screen, signed in or not.
import { createRootRouteWithContext, Outlet } from "@tanstack/react-router";

import { ConnectionBanner } from "../features/shell/components/ConnectionBanner";
import type { RouterContext } from "./context";

/** The root route; every other route hangs below it (router.tsx). */
export const rootRoute = createRootRouteWithContext<RouterContext>()({
  component: RootLayout,
});

function RootLayout() {
  return (
    <div className="flex min-h-screen flex-col bg-background text-foreground">
      <ConnectionBanner />
      <Outlet />
    </div>
  );
}
