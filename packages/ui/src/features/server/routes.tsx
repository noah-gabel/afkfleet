// The server settings' routes (Plan.md P8.7): `/setup` while signed out (the first run, an
// unreadable profile, from the login or start screen) and `/server` in the shell.
import { createRoute } from "@tanstack/react-router";

import { rootRoute } from "../../app/rootRoute";
import { redirectIfSignedIn } from "../shell/guards";
import { appRoute } from "../shell/routes";
import { ServerForm } from "./components/ServerForm";

/** `/setup`: server setup while signed out. */
export const setupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "setup",
  beforeLoad: ({ context }) => redirectIfSignedIn(context, "/server"),
  component: () => <ServerForm title="Server setup" />,
});

/** `/server`: the server settings, signed in. */
export const serverRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "server",
  component: () => <ServerForm title="Server" />,
});
