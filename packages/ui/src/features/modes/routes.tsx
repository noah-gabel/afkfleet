// The modes' routes (Plan.md P11.9; ADR-0020, U74, R165): the list, a new mode's screen and one
// screen per mode, for everyone signed in.
import { createRoute } from "@tanstack/react-router";

import { appRoute } from "../shell/routes";
import { ModeScreen, NewModeScreen } from "./components/ModeScreen";
import { ModesScreen } from "./components/ModesScreen";

/** `/modes`. */
export const modesRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "modes",
  component: ModesScreen,
});

/** `/modes/new`: a new mode. */
export const newModeRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "modes/new",
  component: NewModeScreen,
});

/** `/modes/$id`: one mode. */
export const modeRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "modes/$id",
  component: function ModeRoute() {
    const { id } = modeRoute.useParams();
    return <ModeScreen key={id} id={id} />;
  },
});
