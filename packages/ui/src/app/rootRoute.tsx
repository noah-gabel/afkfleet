// The router's root (Plan.md P8.6, P11.11; ADR-0020, S102, S103): the banner above every screen,
// signed in or not, and under it the live hint while signed in. They stay here, never moving
// between layouts, so the banner's retry rhythm and both status regions survive a login and a logout.
// The stack is sticky, so it stays in view as a screen scrolls, and over the content column when the
// fixed sidebar is there.
import { createRootRouteWithContext, Outlet, useMatches } from "@tanstack/react-router";
import { cn } from "cn";

import { ConnectionBanner } from "../features/shell/components/ConnectionBanner";
import { LiveHint } from "../features/shell/components/LiveHint";
import type { RouterContext } from "./context";

/** The root route; every other route hangs below it (router.tsx). */
export const rootRoute = createRootRouteWithContext<RouterContext>()({
  component: RootLayout,
});

function RootLayout() {
  const inApp = useMatches({
    select: (matches) => matches.some((match) => match.routeId === "/app"),
  });
  return (
    <div className="flex min-h-screen flex-col bg-background text-foreground">
      <div className={cn("sticky top-0 z-20", inApp ? "ml-52 max-rail:ml-16" : undefined)}>
        <ConnectionBanner />
        {inApp ? <LiveHint /> : null}
      </div>
      <Outlet />
    </div>
  );
}
