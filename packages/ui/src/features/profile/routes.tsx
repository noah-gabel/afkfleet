// The profile's routes (Plan.md P8.7): the overview, and one route per flow beside it, so a
// flow's password, secret and codes go when the user leaves it.
import { createRoute } from "@tanstack/react-router";

import { appRoute } from "../shell/routes";
import { ProfileFlow } from "./components/ProfileFlow";
import { ProfileOverview } from "./components/ProfileOverview";

/** `/profile`. */
export const profileRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "profile",
  component: ProfileOverview,
});

/** `/profile/password`: changing the password. */
export const passwordRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "profile/password",
  component: () => <ProfileFlow mode="password" />,
});

/** `/profile/2fa`: replacing the authenticator. */
export const mfaRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "profile/2fa",
  component: () => <ProfileFlow mode="mfa" />,
});
