// The accounts' routes (Plan.md P9.8, ADR-0018 group F): the list, one screen per account,
// and the link screen, a route of its own (like the profile's flows) so its code goes when
// the user leaves it, for a new link and for linking an account again.
import { createRoute } from "@tanstack/react-router";

import { appRoute } from "../shell/routes";
import { AccountScreen } from "./components/AccountScreen";
import { AccountsScreen } from "./components/AccountsScreen";
import { LinkScreen } from "./components/LinkScreen";

/** `/accounts`. */
export const accountsRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "accounts",
  component: AccountsScreen,
});

/** `/accounts/link`: linking a new Microsoft account. */
export const linkRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "accounts/link",
  component: () => <LinkScreen />,
});

/** `/accounts/$id`: one account. */
export const accountRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "accounts/$id",
  component: function AccountRoute() {
    const { id } = accountRoute.useParams();
    return <AccountScreen key={id} id={id} />;
  },
});

/** `/accounts/$id/link-again`: linking an account again. */
export const linkAgainRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "accounts/$id/link-again",
  component: function LinkAgainRoute() {
    const { id } = linkAgainRoute.useParams();
    return <LinkScreen key={id} again={id} />;
  },
});
