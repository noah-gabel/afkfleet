// The zod schemas of the app's inputs (Plan.md P8.5, ADR-0017), mirroring the server's
// limits (limits.ts) so a form can say what's wrong before anything is sent. The server
// validates everything again and stays authoritative. Lengths count Unicode code points
// (chars), as the Rust side does; the server profile's caps count UTF-8 bytes.

import {
  CA_PEM_MAX_BYTES,
  CHAT_FORMAT_MAX_CHARS,
  CHAT_FORMATS_MAX,
  CHAT_MESSAGE_MAX_UNITS,
  CHAT_PREVIEW_LINE_MAX_CHARS,
  CODE_MAX_CHARS,
  CONFLICT_TEXT_MAX_CHARS,
  CONFLICT_TEXTS_MAX,
  ID_CHARS,
  MC_NAME_MAX_CHARS,
  MC_NAME_MIN_CHARS,
  NEW_PASSWORD_MAX_CHARS,
  NEW_PASSWORD_MIN_CHARS,
  PAGE_MAX,
  PASSWORD_MAX_CHARS,
  SERVER_ADDRESS_MAX_CHARS,
  SERVER_SETTINGS_PAGE_MAX,
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

/** A Minecraft name: the server's `McUsername` rule, in its own words. */
export const mcName = z
  .string()
  .refine(
    (text) =>
      text.length >= MC_NAME_MIN_CHARS &&
      text.length <= MC_NAME_MAX_CHARS &&
      /^[A-Za-z0-9_]+$/.test(text),
    `must be ${MC_NAME_MIN_CHARS} to ${MC_NAME_MAX_CHARS} letters, digits or \`_\``,
  );

export const createOfflineAccountRequest = z.strictObject({ name: mcName });

/** A grant's level. */
export const grantLevel = z.enum(["view", "control", "manage"]);

export const createGrantRequest = z.strictObject({ username, level: grantLevel });

export const changeGrantRequest = z.strictObject({ level: grantLevel });

/**
 * A bot's server address: not empty, and at most `SERVER_ADDRESS_MAX_CHARS` characters, in
 * the server's words; the server checks its host and port.
 */
export const serverAddress = z
  .string()
  .refine((text) => text.length > 0, "must not be empty")
  .refine(
    (text) => chars(text) <= SERVER_ADDRESS_MAX_CHARS,
    `must be at most ${SERVER_ADDRESS_MAX_CHARS} characters, with a host of at most 253`,
  );

export const botPatchRequest = z.strictObject({ server_address: serverAddress });

/** A page size of the servers' settings: 1 to `SERVER_SETTINGS_PAGE_MAX`. */
export const serverSettingsLimit = z
  .number()
  .int()
  .min(1)
  .max(SERVER_SETTINGS_PAGE_MAX, `must be between 1 and ${SERVER_SETTINGS_PAGE_MAX}`);

/**
 * One conflict text: at most `CONFLICT_TEXT_MAX_CHARS` characters, in the server's words. The
 * server checks the rest: that it isn't empty, and its characters (ADR-0020, S77).
 */
export const conflictText = z
  .string()
  .refine(
    (text) => chars(text) <= CONFLICT_TEXT_MAX_CHARS,
    `must be at most ${CONFLICT_TEXT_MAX_CHARS} characters`,
  );

/** A server's conflict texts: at most `CONFLICT_TEXTS_MAX`, in the server's words. */
export const conflictTexts = z
  .array(conflictText)
  .refine(
    (texts) => texts.length <= CONFLICT_TEXTS_MAX,
    `must hold at most ${CONFLICT_TEXTS_MAX} texts`,
  );

/**
 * One chat format: at most `CHAT_FORMAT_MAX_CHARS` characters as written, in the server's
 * words. The server checks the rest: its placeholders, braces and characters (ADR-0020, S80).
 */
export const chatFormat = z
  .string()
  .refine(
    (format) => chars(format) <= CHAT_FORMAT_MAX_CHARS,
    `must be at most ${CHAT_FORMAT_MAX_CHARS} characters`,
  );

/** A server's chat formats: at most `CHAT_FORMATS_MAX`, in the server's words. */
export const chatFormats = z
  .array(chatFormat)
  .refine(
    (formats) => formats.length <= CHAT_FORMATS_MAX,
    `must hold at most ${CHAT_FORMATS_MAX} formats`,
  );

export const createServerSettingsRequest = z.strictObject({
  server: serverAddress,
  conflict_texts: conflictTexts,
  chat_formats: chatFormats,
});

export const updateServerSettingsRequest = z.strictObject({
  version: z.number().int().min(1),
  server: serverAddress,
  conflict_texts: conflictTexts,
  chat_formats: chatFormats,
});

/**
 * The chat formats' preview: the formats under the save's rules, and a line as the server sends
 * it, at most `CHAT_PREVIEW_LINE_MAX_CHARS` characters (ADR-0020, R142).
 */
export const previewChatFormatsRequest = z.strictObject({
  chat_formats: chatFormats,
  line: z
    .string()
    .refine(
      (line) => chars(line) <= CHAT_PREVIEW_LINE_MAX_CHARS,
      `must be at most ${CHAT_PREVIEW_LINE_MAX_CHARS} characters`,
    ),
});

export const botModeRequest = z.strictObject({ mode_id: id });

/**
 * A chat page's cursor: a line's ID, an integer of at least 1 that a JavaScript number holds
 * exactly, in the server's words.
 */
export const chatCursor = z
  .number()
  .int()
  .min(1, "lower than 1")
  .max(Number.MAX_SAFE_INTEGER, "greater than 9007199254740991");

/**
 * A chat line, once trimmed: not empty and at most `CHAT_MESSAGE_MAX_UNITS` UTF-16 units, in
 * the server's words (ADR-0020, R117). The server checks its characters; the desktop's own
 * check before the request says the same.
 */
export const chatMessage = z
  .string()
  .refine((text) => text.trim().length > 0, "must not be empty")
  .refine(
    (text) => text.trim().length <= CHAT_MESSAGE_MAX_UNITS,
    `must be at most ${CHAT_MESSAGE_MAX_UNITS} characters, an emoji counting as 2`,
  );

export const chatSendRequest = z.strictObject({ message: chatMessage });

/** A command for `/command`: a chat line that starts with `/` once trimmed (S8). */
export const chatCommandRequest = z.strictObject({
  message: chatMessage.refine(
    (text) => text.trim().startsWith("/"),
    "must be a command, starting with /",
  ),
});
