// The shell's routes (Plan.md P8.6): `/`, whose loader is the launch, and the layout of
// every signed-in screen, which only a signed-in session enters.
import { createRoute } from "@tanstack/react-router";

import { rootRoute } from "../../app/rootRoute";
import { AppLayout } from "./components/AppLayout";
import { NotFound } from "./components/NotFound";
import { StartScreen } from "./components/StartScreen";
import { requireSignedIn } from "./guards";
import { launch } from "./launch";

/** `/`: the launch, then the start screen if it leads nowhere else. */
export const launchRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  loader: ({ context }) => launch(context),
  // Re-run on every visit and on `invalidate()`, never from a cache.
  staleTime: 0,
  gcTime: 0,
  pendingMs: 0,
  pendingMinMs: 0,
  pendingComponent: Connecting,
  component: StartScreen,
});

/** The layout of the signed-in screens: header, navigation, then the screen. */
export const appRoute = createRoute({
  getParentRoute: () => rootRoute,
  id: "app",
  beforeLoad: ({ context }) => requireSignedIn(context),
  component: AppLayout,
  notFoundComponent: NotFound,
});

function Connecting() {
  return (
    <main className="flex flex-1 items-center justify-center p-8">
      <p className="text-muted-foreground">Connecting…</p>
    </main>
  );
}
