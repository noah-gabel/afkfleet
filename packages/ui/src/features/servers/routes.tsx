// The Minecraft servers' routes (Plan.md P11.9; ADR-0020, U57, R136): the list, a new
// server's screen and one screen per server's settings, under the admin layout, whose
// `requireAdmin` lets in exactly who may read them (S75; admin/permissions.ts).
import { createRoute } from "@tanstack/react-router";

import { adminRoute } from "../admin/routes";
import { ServerSettingsScreen } from "./components/ServerSettingsScreen";
import { ServersScreen } from "./components/ServersScreen";

/** `/admin/servers`. */
export const serversRoute = createRoute({
  getParentRoute: () => adminRoute,
  path: "servers",
  component: ServersScreen,
});

/** `/admin/servers/new`: a new server's settings. */
export const newServerRoute = createRoute({
  getParentRoute: () => adminRoute,
  path: "servers/new",
  component: () => <ServerSettingsScreen />,
});

/** `/admin/servers/$id`: one server's settings. */
export const serverSettingsRoute = createRoute({
  getParentRoute: () => adminRoute,
  path: "servers/$id",
  component: function ServerSettingsRoute() {
    const { id } = serverSettingsRoute.useParams();
    return <ServerSettingsScreen key={id} id={id} />;
  },
});
