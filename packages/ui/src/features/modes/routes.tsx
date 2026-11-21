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

/** A whole number from 1, or nothing: a search parameter's line or column. */
function placeNumber(value: unknown): number | undefined {
  const number = typeof value === "string" ? Number(value) : value;
  return typeof number === "number" && Number.isSafeInteger(number) && number > 0
    ? number
    : undefined;
}

/** `/modes/$id`: one mode; `?line=&column=` shows a place in its script (P15.26). */
export const modeRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "modes/$id",
  validateSearch: (search: Record<string, unknown>): { line?: number; column?: number } => {
    const line = placeNumber(search.line);
    const column = placeNumber(search.column);
    return line === undefined ? {} : { line, ...(column === undefined ? {} : { column }) };
  },
  component: function ModeRoute() {
    const { id } = modeRoute.useParams();
    const { line, column } = modeRoute.useSearch();
    const focusAt =
      line === undefined ? null : { line, ...(column === undefined ? {} : { column }) };
    return <ModeScreen key={id} id={id} focusAt={focusAt} />;
  },
});
