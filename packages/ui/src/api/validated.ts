// The zod check in front of every API call (Plan.md P8.5, ADR-0017): `validated(client)`
// parses each input with its schema (schemas.ts) before the inner client sees it, so no
// call with a broken input reaches `invoke`. A failure rejects with `invalid_input`, its
// fields named like garde's (`username`, `limit`, `steps[0].angle`), the same shape the
// Rust side answers. `App` wraps whatever client it's given.
import type { z } from "zod";

import type { ApiClient } from "./ApiClient";
import { invalidInput } from "./ApiError";
import * as schemas from "./schemas";

/** `client` behind the input checks; see the module comment. */
export function validated(client: ApiClient): ApiClient {
  return {
    serverProfile: () => client.serverProfile(),
    setServerProfile: async (request) =>
      client.setServerProfile(check(schemas.serverProfileRequest, request)),
    checkServer: () => client.checkServer(),
    login: async (request) => client.login(check(schemas.loginRequest, request)),
    loginMfa: async (code) => client.loginMfa(check(schemas.code, code, "code")),
    logout: () => client.logout(),
    register: async (request) => client.register(check(schemas.registerRequest, request)),
    stepUp: async (request) => client.stepUp(check(schemas.stepUpRequest, request)),
    mfaEnroll: () => client.mfaEnroll(),
    mfaConfirm: async (code) => client.mfaConfirm(check(schemas.code, code, "code")),
    mfaReset: () => client.mfaReset(),
    me: () => client.me(),
    changePassword: async (request) =>
      client.changePassword(check(schemas.passwordChangeRequest, request)),
    sessions: async (before, limit) => {
      checkPage(before, limit);
      return client.sessions(before, limit);
    },
    revokeSession: async (id) => client.revokeSession(check(schemas.id, id, "id")),
    users: async (before, limit) => {
      checkPage(before, limit);
      return client.users(before, limit);
    },
    patchUser: async (id, request) => {
      checkAll([...problems(schemas.id, id, "id"), ...problems(schemas.userPatchRequest, request)]);
      return client.patchUser(id, request);
    },
    invites: async (before, limit) => {
      checkPage(before, limit);
      return client.invites(before, limit);
    },
    createInvite: async (request) =>
      client.createInvite(check(schemas.createInviteRequest, request)),
    revokeInvite: async (id) => client.revokeInvite(check(schemas.id, id, "id")),
  };
}

/** garde's notation of a zod path, under `name` for a positional argument. */
function pathOf(name: string | undefined, path: readonly PropertyKey[]): string {
  let text = name ?? "";
  for (const key of path) {
    text =
      typeof key === "number"
        ? `${text}[${key}]`
        : text === ""
          ? String(key)
          : `${text}.${String(key)}`;
  }
  return text;
}

/** The fields `value` breaks under `schema`, named under `name`. */
function problems(schema: z.ZodType, value: unknown, name?: string) {
  const result = schema.safeParse(value);
  return result.success
    ? []
    : result.error.issues.map((issue) => ({
        path: pathOf(name, issue.path),
        message: issue.message,
      }));
}

/** Throws `invalid_input` with `fields`, unless there are none. */
function checkAll(fields: ReturnType<typeof problems>): void {
  if (fields.length > 0) {
    throw invalidInput(fields);
  }
}

/** `value` if it passes `schema`; otherwise throws `invalid_input`. */
function check<T>(schema: z.ZodType, value: T, name?: string): T {
  checkAll(problems(schema, value, name));
  return value;
}

/** A page's `before` (an ID, when given) and `limit` (a page size, when given). */
function checkPage(before: string | undefined, limit: number | undefined): void {
  checkAll([
    ...(before === undefined ? [] : problems(schemas.id, before, "before")),
    ...(limit === undefined ? [] : problems(schemas.limit, limit, "limit")),
  ]);
}
