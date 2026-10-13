// The routes' guards (Plan.md P8.6): the signed-in screens need a signed-in session, the
// signed-out ones send a signed-in user on, and the admin screens need an Admin or the
// Owner. The server enforces every rule again; these only decide what the UI shows.
import { notFound, redirect } from "@tanstack/react-router";

import { meQuery } from "../../api/queries";
import type { RouterContext } from "../../app/context";
import { privilegeOf } from "./nav";

/** Sends anyone not signed in to /login. */
export function requireSignedIn({ stores }: RouterContext): void {
  if (stores.session.getState().phase !== "signed_in") {
    throw redirect({ to: "/login" });
  }
}

/** Sends a signed-in user to `to` instead of a signed-out screen. */
export function redirectIfSignedIn({ stores }: RouterContext, to: "/home" | "/server"): void {
  if (stores.session.getState().phase === "signed_in") {
    throw redirect({ to });
  }
}

/**
 * Lets only Admins and the Owner in; anyone else gets "not found", like the server's 404
 * for what an actor can't see. A role this app doesn't know counts as a Member's.
 */
export async function requireAdmin({ queryClient, apiClient }: RouterContext): Promise<void> {
  const user = await queryClient.ensureQueryData(meQuery(apiClient));
  if (privilegeOf(user.role) !== "admin") {
    throw notFound();
  }
}
