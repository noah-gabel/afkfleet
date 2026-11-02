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
    message: "Passwords don't match.",
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

/** Under the boxes, saying they send by themselves (WCAG 3.2.2). */
export const CODE_HINT = "6 digits from your app. Sends when full.";
/** Verify, before all six are there. */
export const ALL_SIX = "Enter all 6 digits.";
/** A letter typed where a recovery code could go. */
export const DIGITS_ONLY = "Digits only. For a recovery code, use the link below.";
/** Anything else left out of the boxes. */
export const NOT_A_CODE = "Not a 6-digit code.";

/** What came into the boxes. */
export type CodeInput =
  | { readonly kind: "digits"; readonly digits: string }
  | { readonly kind: "recovery"; readonly code: string }
  | { readonly kind: "other" };

/**
 * What `text` is, ignoring the spaces and hyphens that apps and chat windows add: up to 6 digits, a
 * whole recovery code (20 base32 characters), or neither. Only whole readings: filtering a recovery
 * code down to its digits could make 6, and sending that would cost a try (ADR-0020, S100).
 */
export function classifyCode(text: string): CodeInput {
  const plain = text.replace(/[\s-]+/g, "");
  if (/^[0-9]{0,6}$/.test(plain)) {
    return { kind: "digits", digits: plain };
  }
  return /^[a-z2-7]{20}$/i.test(plain)
    ? { kind: "recovery", code: text.trim() }
    : { kind: "other" };
}
