// Step-up (Plan.md §7.2, P8.7, ADR-0016, ADR-0028): the password again, which the server
// accepts for 5 minutes. Only changing the password and replacing the authenticator need it;
// without it they answer 403 `step_up_required`. Nothing else asks for the password again.
import type { CommandError } from "../generated/CommandError";

/** A wrong password at step-up: the server answers 403 `forbidden`. */
export const WRONG_PASSWORD = "Wrong password.";

/** The server wants the password again first. */
export function isStepUpRequired(error: CommandError): boolean {
  return error.api?.status === 403 && error.api.code === "step_up_required";
}

/** A step-up's wrong password. */
export function isWrongPassword(error: CommandError): boolean {
  return error.api?.status === 403 && error.api.code === "forbidden";
}
