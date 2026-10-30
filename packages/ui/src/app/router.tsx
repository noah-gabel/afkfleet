// The app's routes (Plan.md P8.6, ADR-0017), defined in code, over a memory history: the
// URL never changes, and a reload or a restart always starts at `/`, whose launch decides
// where to go.
//
//   /                launch: server profile, then the session (features/shell/launch.ts)
//   /setup           server setup while signed out
//   /login /register the sign-in flows
//   (app)            signed in only: header, navigation
//     /home          the bots' dashboard
//       /bots/$id    one bot
//     /server
//     /modes /modes/new /modes/$id   the modes the user may see, a new one, one
//     /accounts      the Microsoft accounts the user may see
//       /accounts/link /accounts/$id /accounts/$id/link-again
//     /profile       the user's own account, sessions and warnings
//       /profile/password /profile/2fa   one route per flow, beside the overview
//     /admin         Admins and the Owner only; anyone else gets "not found"
//       /admin/users /admin/invites
//       /admin/servers /admin/servers/new /admin/servers/$id   the Minecraft servers' settings
import { createMemoryHistory, createRouter } from "@tanstack/react-router";

import {
  accountRoute,
  accountsRoute,
  linkAgainRoute,
  linkRoute,
} from "../features/accounts/routes";
import { adminRoute, invitesRoute, usersRoute } from "../features/admin/routes";
import { loginRoute, registerRoute } from "../features/auth/routes";
import { botRoute } from "../features/bots/routes";
import { homeRoute } from "../features/home/routes";
import { modeRoute, modesRoute, newModeRoute } from "../features/modes/routes";
import { mfaRoute, passwordRoute, profileRoute } from "../features/profile/routes";
import { serverRoute, setupRoute } from "../features/server/routes";
import { newServerRoute, serverSettingsRoute, serversRoute } from "../features/servers/routes";
import { NotFound } from "../features/shell/components/NotFound";
import { RouteError } from "../features/shell/components/RouteError";
import { appRoute, launchRoute } from "../features/shell/routes";
import type { RouterContext } from "./context";
import { rootRoute } from "./rootRoute";

const routeTree = rootRoute.addChildren([
  launchRoute,
  setupRoute,
  loginRoute,
  registerRoute,
  appRoute.addChildren([
    homeRoute,
    botRoute,
    modesRoute,
    newModeRoute,
    modeRoute,
    accountsRoute,
    linkRoute,
    accountRoute,
    linkAgainRoute,
    serverRoute,
    profileRoute,
    passwordRoute,
    mfaRoute,
    adminRoute.addChildren([
      usersRoute,
      invitesRoute,
      serversRoute,
      newServerRoute,
      serverSettingsRoute,
    ]),
  ]),
]);

/** A router for one App, with its own history, starting at `/`. */
export function createAppRouter(context: RouterContext) {
  return createRouter({
    routeTree,
    context,
    history: createMemoryHistory({ initialEntries: ["/"] }),
    defaultErrorComponent: RouteError,
    defaultNotFoundComponent: NotFound,
  });
}

/** The app's router. */
export type AppRouter = ReturnType<typeof createAppRouter>;

declare module "@tanstack/react-router" {
  interface Register {
    router: AppRouter;
  }
}
