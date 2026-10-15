// How the app's API fails (Plan.md P8.5, ADR-0017): every `ApiClient` method rejects with
// an `ApiError` carrying the `CommandError` the Rust side answered. Its `code` is closed
// (the Rust side ships with this UI), so the UI switches on it exhaustively; its
// `api.code` is the server's, which can grow, so a switch on it always has a default
// branch that shows `api.message` and `api.request_id` (describeError.ts).

import type { CommandError } from "../generated/CommandError";
import type { CommandErrorCode } from "../generated/CommandErrorCode";
import type { FieldError } from "../generated/FieldError";
import { z } from "./zod";

/**
 * Every code this UI and its Rust side know. A `Record` over the generated union, so the
 * compiler fails when a code is added to or removed from the Rust enum but not here.
 */
const CODES: Record<CommandErrorCode, true> = {
  unreachable: true,
  certificate: true,
  timed_out: true,
  too_large: true,
  unexpected_response: true,
  api: true,
  no_session: true,
  no_pending_step: true,
  already_logged_in: true,
  store: true,
  interrupted: true,
  no_server: true,
  invalid_input: true,
  invalid_server_url: true,
  invalid_ca: true,
  client_build: true,
  profile_save: true,
  session_discard: true,
  invalid_verification_link: true,
  no_verification_link: true,
  verification_link_open: true,
  ipc: true,
};

/** `invalid_input`'s fixed sentence: `CommandErrorCode::InvalidInput.message()`'s. */
export const INVALID_INPUT_MESSAGE = "Some inputs are invalid.";

/** `ipc`'s fixed sentence: `CommandErrorCode::Ipc.message()`'s. */
export const IPC_MESSAGE = "The app couldn't run the request.";

const fieldError = z.object({ path: z.string(), message: z.string() });

/** A `CommandError` as the Rust side serializes it. */
const commandError = z.object({
  code: z.string().refine((code) => Object.hasOwn(CODES, code)),
  message: z.string(),
  api: z
    .object({
      status: z.number().int(),
      code: z.string(),
      message: z.string(),
      request_id: z.string(),
      fields: z.array(fieldError).optional(),
      retry_after_secs: z.number().int().nonnegative().optional(),
    })
    .optional(),
  fields: z.array(fieldError).optional(),
});

/** Whether `value` is a `CommandError` with a code this UI knows. */
export function isCommandError(value: unknown): value is CommandError {
  return commandError.safeParse(value).success;
}

/** A failed API call. */
export class ApiError extends Error {
  /** What the Rust side answered, or what this UI made of a rejection it couldn't read. */
  readonly error: CommandError;

  constructor(error: CommandError) {
    super(error.message);
    this.name = "ApiError";
    this.error = error;
  }
}

/**
 * The `ApiError` for anything an API call rejected with: a `CommandError` as it came,
 * anything else (Tauri's own refusals are plain strings) as `ipc`, its text dropped.
 */
export function toApiError(rejection: unknown): ApiError {
  if (rejection instanceof ApiError) {
    return rejection;
  }
  return new ApiError(
    isCommandError(rejection) ? rejection : { code: "ipc", message: IPC_MESSAGE },
  );
}

/** The `ApiError` for inputs that broke the rules `fields` name. */
export function invalidInput(fields: FieldError[]): ApiError {
  return new ApiError({ code: "invalid_input", message: INVALID_INPUT_MESSAGE, fields });
}
