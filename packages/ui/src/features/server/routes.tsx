// The server settings' routes (Plan.md P8.7): `/setup` while signed out (the first run, an
// unreadable profile, from the login or start screen) and `/server` in the shell. Opened from the
// start screen (`?from=start`), the setup's way back returns there, which runs the launch again:
// the session this PC holds is still there (ADR-0022, U7).
import { createRoute } from "@tanstack/react-router";

import { rootRoute } from "../../app/rootRoute";
import { redirectIfSignedIn } from "../shell/guards";
import { appRoute } from "../shell/routes";
import { ServerForm } from "./components/ServerForm";

/** The setup route's search: where it was opened from, when that was the start screen. */
export type SetupSearch = { readonly from: "start" } | Record<string, never>;

/** The search `search` asks for; anything else is none. */
export function setupSearch(search: Record<string, unknown>): SetupSearch {
  return search.from === "start" ? { from: "start" } : {};
}

/** `/setup`: server setup while signed out. */
export const setupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "setup",
  validateSearch: setupSearch,
  beforeLoad: ({ context }) => redirectIfSignedIn(context, "/server"),
  component: function SetupRoute() {
    // The router keeps a search's keys its `validateSearch` left out: only what passes here counts.
    const search = setupSearch(setupRoute.useSearch());
    return <ServerForm title="Server setup" backTo={search.from === "start" ? "start" : "login"} />;
  },
});

/** `/server`: the server settings, signed in. */
export const serverRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "server",
  component: () => <ServerForm title="Server" backTo="login" />,
});
