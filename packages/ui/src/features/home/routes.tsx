// Home's route (Plan.md P8.6): the first signed-in screen.
import { createRoute } from "@tanstack/react-router";

import { appRoute } from "../shell/routes";
import { Home } from "./components/Home";

/** `/home`. */
export const homeRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "home",
  component: Home,
});
