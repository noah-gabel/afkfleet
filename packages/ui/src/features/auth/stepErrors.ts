// What a failed call means for a step of a flow (Plan.md P8.7, ADR-0017), shared by the
// sign-in flow and the profile's authenticator replacement.
import type { CommandError } from "../../generated/CommandError";

/** A setup or code step the server or the client no longer holds. */
export function isStepGone(error: CommandError): boolean {
  return error.code === "no_pending_step" || error.api?.status === 401;
}
