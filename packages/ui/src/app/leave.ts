// Leaving the signed-in screens (Plan.md P8.6, ADR-0017): a logout, a server switch and a
// session that ended during use all go to /login first and only then empty the cache, so
// no screen still mounted refetches its data after it's gone (which, for a session end,
// would answer `no_session` all over again).
import type { QueryClient } from "@tanstack/react-query";
import { toast } from "sonner";

import type { AppRouter } from "./router";

/** The id of the "session ended" toast, so it shows once however many calls failed. */
export const SESSION_ENDED_TOAST_ID = "session-ended";

/** The "session ended" toast's text. */
export const SESSION_ENDED = "Your session ended. Log in again.";

/** Goes to /login, then empties the cache; see the module comment. */
export async function leaveSession(router: AppRouter, queryClient: QueryClient): Promise<void> {
  await router.navigate({ to: "/login" });
  queryClient.clear();
}

/** A session that ended during use: leaves it, then says so once. */
export async function endSession(router: AppRouter, queryClient: QueryClient): Promise<void> {
  await leaveSession(router, queryClient);
  toast.warning(SESSION_ENDED, { id: SESSION_ENDED_TOAST_ID });
}
