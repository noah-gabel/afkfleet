// The zod schemas of the app's inputs (Plan.md P8.5, ADR-0017), mirroring the server's
// limits (limits.ts) so a form can say what's wrong before anything is sent. The server
// validates everything again and stays authoritative. Lengths count Unicode code points
// (chars), as the Rust side does; the server profile's caps count UTF-8 bytes.

import {
  CA_PEM_MAX_BYTES,
  CODE_MAX_CHARS,
  ID_CHARS,
  NEW_PASSWORD_MAX_CHARS,
  NEW_PASSWORD_MIN_CHARS,
  PAGE_MAX,
  PASSWORD_MAX_CHARS,
  SERVER_URL_MAX_BYTES,
  TOKEN_MAX_CHARS,
  USERNAME_MAX_CHARS,
  USERNAME_MIN_CHARS,
} from "./limits";
import { z } from "./zod";

/** How many Unicode code points `text` holds. */
function chars(text: string): number {
  return [...text].length;
}

/** How many UTF-8 bytes `text` takes. */
function bytes(text: string): number {
  return new TextEncoder().encode(text).length;
}

/** Whether `text` holds a control character (C0, DEL or C1). */
function hasControl(text: string): boolean {
  return [...text].some((character) => {
    const point = character.codePointAt(0) ?? 0;
    return point < 0x20 || (point >= 0x7f && point <= 0x9f);
  });
}

const USERNAME_RULE = `must be ${USERNAME_MIN_CHARS} to ${USERNAME_MAX_CHARS} letters, digits, \`_\`, \`.\` or \`-\`, starting with a letter or digit`;

/** A username: the server's `Username` rule. */
export const username = z
  .string()
  .refine(
    (text) =>
      chars(text) >= USERNAME_MIN_CHARS &&
      chars(text) <= USERNAME_MAX_CHARS &&
      /^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(text),
    USERNAME_RULE,
  );

/** A password at login or step-up: only capped, so no rule leaks what a password is like. */
export const password = z
  .string()
  .refine(
    (text) => chars(text) <= PASSWORD_MAX_CHARS,
    `length must be at most ${PASSWORD_MAX_CHARS}`,
  );

/** A new password: the server's length and character rules; it checks the strength. */
export const newPassword = z
  .string()
  .refine(
    (text) => chars(text) >= NEW_PASSWORD_MIN_CHARS && chars(text) <= NEW_PASSWORD_MAX_CHARS,
    `must be ${NEW_PASSWORD_MIN_CHARS} to ${NEW_PASSWORD_MAX_CHARS} characters`,
  )
  .refine((text) => !hasControl(text), "must not hold control characters");

/** A code from the authenticator app, or a recovery code; the server checks its shape. */
export const code = z
  .string()
  .refine(
    (text) => chars(text) >= 1 && chars(text) <= CODE_MAX_CHARS,
    `length must be between 1 and ${CODE_MAX_CHARS}`,
  );

/** An ID: a version 7 UUID in its hyphenated form, as the server writes it. */
export const id = z
  .string()
  .refine(
    (text) =>
      text.length === ID_CHARS &&
      /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-7[0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}$/.test(
        text,
      ),
    "must be an ID",
  );

/** A page size. */
export const limit = z.number().int().min(1).max(PAGE_MAX, `must be between 1 and ${PAGE_MAX}`);

const role = z.enum(["member", "admin"]);

export const loginRequest = z.strictObject({ username, password });

export const registerRequest = z.strictObject({
  invite: z
    .string()
    .refine((text) => chars(text) <= TOKEN_MAX_CHARS, `length must be at most ${TOKEN_MAX_CHARS}`),
  username,
  password: newPassword,
});

export const stepUpRequest = z.strictObject({ password });

export const passwordChangeRequest = z.strictObject({ new_password: newPassword });

export const userPatchRequest = z
  .strictObject({ role: role.optional(), disabled: z.boolean().optional() })
  .refine(
    (patch) => (patch.role === undefined) !== (patch.disabled === undefined),
    "must hold exactly one of `role` and `disabled`",
  );

export const createInviteRequest = z.strictObject({ role });

export const serverProfileRequest = z.strictObject({
  url: z
    .string()
    .refine(
      (text) => bytes(text) <= SERVER_URL_MAX_BYTES,
      `length must be at most ${SERVER_URL_MAX_BYTES} bytes`,
    ),
  ca_pem: z
    .string()
    .refine(
      (text) => bytes(text) <= CA_PEM_MAX_BYTES,
      `length must be at most ${CA_PEM_MAX_BYTES} bytes`,
    )
    .optional(),
});
