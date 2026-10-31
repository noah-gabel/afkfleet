// The admin routes (Plan.md P8.6, P8.8): Admins and the Owner only. Anyone else gets "not
// found", like the server's 404 for what an actor can't see.
import { createRoute } from "@tanstack/react-router";

import { requireAdmin } from "../shell/guards";
import { appRoute } from "../shell/routes";
import { AgentsScreen } from "./components/AgentsScreen";
import { GrantsReviewScreen } from "./components/GrantsReviewScreen";
import { InvitesScreen } from "./components/InvitesScreen";
import { UsersScreen } from "./components/UsersScreen";

/** `/admin`: the role guard of everything below it. */
export const adminRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "admin",
  beforeLoad: ({ context }) => requireAdmin(context),
});

/** `/admin/users`. */
export const usersRoute = createRoute({
  getParentRoute: () => adminRoute,
  path: "users",
  component: UsersScreen,
});

/** `/admin/invites`. */
export const invitesRoute = createRoute({
  getParentRoute: () => adminRoute,
  path: "invites",
  component: InvitesScreen,
});

/** `/admin/agents`. */
export const agentsRoute = createRoute({
  getParentRoute: () => adminRoute,
  path: "agents",
  component: AgentsScreen,
});

/** `/admin/users/$id/grants`: the Owner's review of a user's grants (ADR-0020, U79). */
export const grantsReviewRoute = createRoute({
  getParentRoute: () => adminRoute,
  path: "users/$id/grants",
  component: function GrantsReviewRoute() {
    const { id } = grantsReviewRoute.useParams();
    return <GrantsReviewScreen key={id} id={id} />;
  },
});
