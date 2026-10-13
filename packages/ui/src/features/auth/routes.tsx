// The sign-in routes (Plan.md P8.7): signed out only; a signed-in user goes Home.
import { createRoute } from "@tanstack/react-router";

import { rootRoute } from "../../app/rootRoute";
import { redirectIfSignedIn } from "../shell/guards";

/** `/login`. */
export const loginRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "login",
  beforeLoad: ({ context }) => redirectIfSignedIn(context, "/home"),
  component: () => <Placeholder title="Log in" />,
});

/** `/register`. */
export const registerRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "register",
  beforeLoad: ({ context }) => redirectIfSignedIn(context, "/home"),
  component: () => <Placeholder title="Register" />,
});

// Until P8.7's AuthFlow (the next commit).
function Placeholder({ title }: { title: string }) {
  return (
    <main className="p-8">
      <h1 className="text-2xl font-semibold">{title}</h1>
    </main>
  );
}
