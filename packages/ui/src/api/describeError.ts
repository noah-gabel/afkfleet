// One line of text for a failed call (Plan.md P8.5, ADR-0015, ADR-0017). It switches
// exhaustively on the closed `code`, so a new code doesn't compile until it's handled
// here, and keeps a default branch on the server's open `api.code`, which shows the
// server's message and the request ID: an older app can receive a code added later.
// Group D builds its screens' texts on it.
import type { CommandError } from "../generated/CommandError";

/** The text to show for `error`; see the module comment. */
export function describeError(error: CommandError): string {
  switch (error.code) {
    case "api":
      return describeRefusal(error);
    case "unreachable":
    case "certificate":
    case "timed_out":
    case "too_large":
    case "unexpected_response":
    case "no_session":
    case "no_pending_step":
    case "already_logged_in":
    case "store":
    case "interrupted":
    case "no_server":
    case "invalid_input":
    case "invalid_server_url":
    case "invalid_ca":
    case "client_build":
    case "profile_save":
    case "session_discard":
    case "invalid_verification_link":
    case "no_verification_link":
    case "verification_link_open":
    case "ipc":
      return error.message;
    default: {
      // Every code is handled: a new one is a compile error here.
      const unhandled: never = error.code;
      return unhandled;
    }
  }
}

/** The text of the server's refusal: its own message, with its wait or its request ID. */
function describeRefusal(error: CommandError): string {
  const api = error.api;
  if (api === undefined) {
    return error.message;
  }
  switch (api.code) {
    case "rate_limited":
    case "busy":
      return api.retry_after_secs === undefined
        ? api.message
        : `${api.message} Try again in ${api.retry_after_secs} s.`;
    default:
      // A code this app may not know: the server's message and the request ID (ADR-0015).
      return `${api.message} (request ${api.request_id})`;
  }
}
