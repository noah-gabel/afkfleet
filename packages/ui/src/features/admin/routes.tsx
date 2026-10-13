// The admin routes (Plan.md P8.6, P8.8): Admins and the Owner only. Anyone else gets "not
// found", like the server's 404 for what an actor can't see. Group E replaces the
// placeholders with the users and invites screens.
import { createRoute } from "@tanstack/react-router";

import { requireAdmin } from "../shell/guards";
import { appRoute } from "../shell/routes";
import { AdminPlaceholder } from "./components/AdminPlaceholder";

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
  component: () => <AdminPlaceholder what="users" />,
});

/** `/admin/invites`. */
export const invitesRoute = createRoute({
  getParentRoute: () => adminRoute,
  path: "invites",
  component: () => <AdminPlaceholder what="invites" />,
});
