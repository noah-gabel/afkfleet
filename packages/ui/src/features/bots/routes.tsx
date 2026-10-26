// The bots' routes (Plan.md P11.9): one screen per bot.
import { createRoute } from "@tanstack/react-router";

import { appRoute } from "../shell/routes";
import { BotScreen } from "./components/BotScreen";

/** `/bots/$id`: one bot. */
export const botRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "bots/$id",
  component: function BotRoute() {
    const { id } = botRoute.useParams();
    return <BotScreen key={id} id={id} />;
  },
});
