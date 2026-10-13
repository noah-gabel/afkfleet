// Saving the server profile (Plan.md P8.7, ADR-0017). The Rust side checks the new server
// before changing anything; any change but an identical profile logs out of the old one.
// - `none`: nothing changed, the session stays.
// - `switched`: signed in, the session ends quietly (to /login first, then the cache), and
//   a logout the old server couldn't confirm says so. Signed out, the login comes next.
// - `profile_save` or `session_discard`: they come only after the old server's logout, so a
//   signed-in user is logged out with the error; the previous server stays set.
// - anything else changed nothing: the form shows it.
// A profile holds no secret (an origin and a CA's certificates), so this is a mutation.
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useRouter } from "@tanstack/react-router";
import { toast } from "sonner";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { serverProfileQuery } from "../../../api/queries";
import { useAppStores } from "../../../app/context";
import { leaveSession } from "../../../app/leave";
import type { CommandError } from "../../../generated/CommandError";
import type { ServerProfileRequest } from "../../../generated/ServerProfileRequest";
import type { ServerProfileState } from "../../../generated/ServerProfileState";
import { unconfirmedLogout } from "../../shell/hooks/useLogout";

/** The toast when saving changed nothing. */
export const UNCHANGED = "Nothing changed; you're still logged in.";

/** The toast when a switch logged out but couldn't finish. */
export function switchFailed(error: CommandError): string {
  return `You were logged out, but the new server couldn't be saved: ${describeError(error)} The previous server is still set.`;
}

/** Saving the profile; see the module comment. */
export function useSaveServer() {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const router = useRouter();
  const { session } = useAppStores();
  const mutation = useMutation({
    mutationFn: (request: ServerProfileRequest) => api.setServerProfile(request),
  });

  /** Ends the session quietly: the switch has already logged out. */
  async function leave(): Promise<void> {
    session.getState().signOut();
    await leaveSession(router, queryClient);
  }

  /** Saves `request`; answers the error the form should show, or nothing once it's done. */
  async function save(request: ServerProfileRequest): Promise<CommandError | undefined> {
    const signedIn = session.getState().phase === "signed_in";
    let answer: Awaited<ReturnType<typeof api.setServerProfile>>;
    try {
      answer = await mutation.mutateAsync(request);
    } catch (rejection) {
      const error = toApiError(rejection).error;
      if (signedIn && (error.code === "profile_save" || error.code === "session_discard")) {
        await leave();
        toast.error(switchFailed(error));
        return undefined;
      }
      return error;
    }
    const state: ServerProfileState = { state: "set", profile: answer.profile };
    queryClient.setQueryData(serverProfileQuery(api).queryKey, state);
    if (answer.change === "none") {
      if (signedIn) {
        toast.info(UNCHANGED);
      } else {
        await router.navigate({ to: "/login" });
      }
      return undefined;
    }
    if (signedIn) {
      await leave();
    } else {
      await router.navigate({ to: "/login" });
    }
    if (answer.logout.server === "unconfirmed") {
      toast.warning(unconfirmedLogout(answer.logout.error));
    }
    return undefined;
  }

  return { save, pending: mutation.isPending };
}
