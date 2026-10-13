// What the app does at launch (Plan.md P8.6, ADR-0017), as `/`'s loader:
// - the server profile: none or unreadable → server setup (which says why);
// - then the session, through `me()` (which refreshes first after a start): Ok → signed in
//   with a new generation, then Home; `no_session` → login, with no "session ended" toast,
//   since at launch it only means not logged in;
// - `unreachable` or `timed_out` → the start screen, while the banner retries; its
//   recovery re-runs this loader;
// - anything else → the start screen with the error and a way out (Try again, Server
//   settings, Log out), so a session the app can't use never leaves the user stuck.
// Both answers are fetched fresh and cached for the screens that follow.
import { redirect } from "@tanstack/react-router";

import { toApiError } from "../../api/ApiError";
import { meQuery, serverProfileQuery } from "../../api/queries";
import type { RouterContext } from "../../app/context";
import type { CommandError } from "../../generated/CommandError";

/** What the start screen shows when the launch doesn't lead elsewhere. */
export type LaunchOutcome =
  | { readonly kind: "unreachable" }
  | { readonly kind: "error"; readonly error: CommandError };

/**
 * The launch; see the module comment. It throws the router's redirect when it leads
 * elsewhere.
 */
export async function launch({
  queryClient,
  apiClient,
  stores,
}: RouterContext): Promise<LaunchOutcome> {
  let state: Awaited<ReturnType<typeof apiClient.serverProfile>>;
  try {
    state = await queryClient.fetchQuery({ ...serverProfileQuery(apiClient), staleTime: 0 });
  } catch (rejection) {
    return { kind: "error", error: toApiError(rejection).error };
  }
  if (state.state !== "set") {
    stores.session.getState().signOut();
    throw redirect({ to: "/setup" });
  }
  try {
    await queryClient.fetchQuery({ ...meQuery(apiClient), staleTime: 0 });
  } catch (rejection) {
    const error = toApiError(rejection).error;
    switch (error.code) {
      case "no_session":
        stores.session.getState().signOut();
        throw redirect({ to: "/login" });
      case "unreachable":
      case "timed_out":
        return { kind: "unreachable" };
      default:
        return { kind: "error", error };
    }
  }
  stores.session.getState().signIn();
  throw redirect({ to: "/home" });
}
