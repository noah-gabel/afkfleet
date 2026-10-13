// The sign-in forms' zod schemas (Plan.md P8.7), built on the API's own (api/schemas.ts),
// so a form says what's wrong before anything is sent; the server checks again.

import { loginRequest, newPassword, registerRequest, username } from "../../api/schemas";
import { z } from "../../api/zod";

/** The login form: the API's login request. */
export const loginForm = loginRequest;

/** The login form's values. */
export type LoginValues = z.infer<typeof loginForm>;

/**
 * The register form: the invite trimmed of the spaces and line breaks a chat app adds
 * around a pasted code (ADR-0016), and the new password typed twice; the repeat is never
 * sent.
 */
export const registerForm = z
  .object({
    invite: z
      .string()
      .trim()
      .refine((text) => text !== "", "Enter your invite code.")
      .pipe(registerRequest.shape.invite),
    username,
    password: newPassword,
    repeat: z.string(),
  })
  .refine((values) => values.password === values.repeat, {
    path: ["repeat"],
    message: "The passwords don't match.",
  });

/** The register form's values as typed. */
export type RegisterInput = z.input<typeof registerForm>;
/** The register form's values once checked: the invite trimmed. */
export type RegisterValues = z.output<typeof registerForm>;

/** A code field: anything but blank; the API's check and the server's do the rest. */
export const codeForm = z.object({
  code: z.string().refine((text) => withoutSpaces(text) !== "", "Enter the code."),
});

/** A code field's values. */
export type CodeValues = z.infer<typeof codeForm>;

/** `code` without any whitespace: apps show `123 456`, and pastes carry line breaks. */
export function withoutSpaces(code: string): string {
  return code.replace(/\s+/g, "");
}
