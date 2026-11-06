// The router's root (Plan.md P8.6, P11.11; ADR-0020, S102, S103, S123): the banner above every
// screen, signed in or not, and under it the live hint while signed in. They stay here, never moving
// between layouts, so the banner's retry rhythm and both status regions survive a login and a logout.
// The root is exactly the window's height and never scrolls, so the stack stays in view as a plain top
// row, over the content column when the fixed sidebar is there. What's under it scrolls by itself: a
// sign-in step here, and the content column on a signed-in screen (AppLayout.tsx).
import { createRootRouteWithContext, Outlet, useMatches } from "@tanstack/react-router";
import { cn } from "cn";

import { ConnectionBanner } from "../features/shell/components/ConnectionBanner";
import { LiveHint } from "../features/shell/components/LiveHint";
import { UpdateNotice } from "../features/updates/components/UpdateNotice";
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
    <div className="flex h-dvh flex-col bg-background text-foreground">
      <div className={cn("z-20 shrink-0", inApp ? "ml-52 max-rail:ml-16" : undefined)}>
        <ConnectionBanner />
        <UpdateNotice />
        {inApp ? <LiveHint /> : null}
      </div>
      <div data-slot="screen" className="relative flex min-h-0 flex-1 flex-col overflow-auto">
        <Outlet />
      </div>
    </div>
  );
}
