// The sign-in routes (Plan.md P8.7): signed out only; a signed-in user goes Home.
import { createRoute } from "@tanstack/react-router";

import { rootRoute } from "../../app/rootRoute";
import { redirectIfSignedIn } from "../shell/guards";
import { AuthFlow } from "./components/AuthFlow";

/** `/login`: the sign-in flow from the login form. */
export const loginRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "login",
  beforeLoad: ({ context }) => redirectIfSignedIn(context, "/home"),
  component: () => <AuthFlow mode="login" />,
});

/** `/register`: the sign-in flow from the register form. */
export const registerRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "register",
  beforeLoad: ({ context }) => redirectIfSignedIn(context, "/home"),
  component: () => <AuthFlow mode="register" />,
});
