// A deliberate logout (Plan.md P8.6, ADR-0017), from the header or the start screen. While
// it runs the session is `logging_out`, so a `no_session` answer from a call still in
// flight ends nothing, and after it the session is `signed_out` until the next sign-in.
// `logout()` always ends the session in the app; when it couldn't tell the server, a
// warning says the server's session may live until it expires. A rejection (the call never
// ran, `ipc`) leaves the user signed in, with the error.
import { useQueryClient } from "@tanstack/react-query";
import { useRouter } from "@tanstack/react-router";
import { useState } from "react";
import { toast } from "sonner";
import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { useAppStores } from "../../../app/context";
import { leaveSession } from "../../../app/leave";
import type { CommandError } from "../../../generated/CommandError";

/** The warning after a logout the server couldn't confirm. */
export function unconfirmedLogout(error: CommandError): string {
  return `You're logged out on this PC, but the server couldn't be told (${describeError(error)}), so your session there may stay valid until it expires.`;
}

/** Logging out; see the module comment. */
export function useLogout() {
  const api = useApiClient();
  const router = useRouter();
  const queryClient = useQueryClient();
  const { session } = useAppStores();
  const [pending, setPending] = useState(false);

  /** True once logged out; false when the call failed (the error was shown). */
  async function logOut(): Promise<boolean> {
    const signedIn = session.getState().phase === "signed_in";
    if (signedIn) {
      session.getState().beginLogout();
    }
    setPending(true);
    try {
      const answer = await api.logout();
      session.getState().signOut();
      await leaveSession(router, queryClient);
      if (answer.server === "unconfirmed") {
        toast.warning(unconfirmedLogout(answer.error));
      }
      return true;
    } catch (rejection) {
      if (signedIn) {
        session.getState().cancelLogout();
      }
      toast.error(`Couldn't log out: ${describeError(toApiError(rejection).error)} Try again.`);
      return false;
    } finally {
      setPending(false);
    }
  }

  return { logOut, pending };
}
