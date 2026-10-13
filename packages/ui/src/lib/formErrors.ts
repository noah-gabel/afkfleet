// Putting a failed call's field errors on its form (Plan.md P8.7): the rules a local check
// broke (`CommandError.fields`, garde's paths) or the server's 422 (`api.fields`) go next
// to the inputs they name. Paths are the request's (`ca_pem`); `paths` maps them to the
// form's fields.
import type { FieldValues, Path, UseFormSetError } from "react-hook-form";

import type { CommandError } from "../generated/CommandError";
import type { FieldError } from "../generated/FieldError";

/** The field errors `error` carries, wherever they came from. */
export function fieldErrorsOf(error: CommandError): readonly FieldError[] {
  return error.fields ?? error.api?.fields ?? [];
}

/**
 * Sets each of `error`'s field errors on the form field `paths` maps its path to. True if
 * any landed, so the caller shows the general text only when none did.
 */
export function showFieldErrors<T extends FieldValues>(
  setError: UseFormSetError<T>,
  error: CommandError,
  paths: Readonly<Record<string, Path<T>>>,
): boolean {
  let shown = false;
  for (const field of fieldErrorsOf(error)) {
    const path = Object.hasOwn(paths, field.path) ? paths[field.path] : undefined;
    if (path !== undefined) {
      setError(path, { type: "server", message: field.message });
      shown = true;
    }
  }
  return shown;
}
