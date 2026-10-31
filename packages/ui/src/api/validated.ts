// The zod check in front of every API call (Plan.md P8.5, ADR-0017): `validated(client)`
// parses each input with its schema (schemas.ts) before the inner client sees it, so no
// call with a broken input reaches `invoke`. A failure rejects with `invalid_input`, its
// fields named like garde's (`username`, `limit`, `steps[0].angle`), the same shape the
// Rust side answers. `App` wraps whatever client it's given.

import type { ApiClient } from "./ApiClient";
import { invalidInput } from "./ApiError";
import * as schemas from "./schemas";
import type { z } from "./zod";

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
    startLink: () => client.startLink(),
    linkStatus: async (id) => client.linkStatus(check(schemas.id, id, "id")),
    createOfflineAccount: async (request) =>
      client.createOfflineAccount(check(schemas.createOfflineAccountRequest, request)),
    accounts: async (before, limit) => {
      checkPage(before, limit);
      return client.accounts(before, limit);
    },
    accountsAwaitingApproval: async (before, limit) => {
      checkPage(before, limit);
      return client.accountsAwaitingApproval(before, limit);
    },
    account: async (id) => client.account(check(schemas.id, id, "id")),
    deleteAccount: async (id) => client.deleteAccount(check(schemas.id, id, "id")),
    approveAccount: async (id) => client.approveAccount(check(schemas.id, id, "id")),
    refuseAccount: async (id) => client.refuseAccount(check(schemas.id, id, "id")),
    relinkAccount: async (id) => client.relinkAccount(check(schemas.id, id, "id")),
    accountGrants: async (id, before, limit) => {
      checkAll([...problems(schemas.id, id, "id"), ...pageProblems(before, limit)]);
      return client.accountGrants(id, before, limit);
    },
    createGrant: async (id, request) => {
      checkAll([
        ...problems(schemas.id, id, "id"),
        ...problems(schemas.createGrantRequest, request),
      ]);
      return client.createGrant(id, request);
    },
    changeGrant: async (id, user, request) => {
      checkAll([
        ...problems(schemas.id, id, "id"),
        ...problems(schemas.id, user, "user"),
        ...problems(schemas.changeGrantRequest, request),
      ]);
      return client.changeGrant(id, user, request);
    },
    revokeGrant: async (id, user) => {
      checkAll([...problems(schemas.id, id, "id"), ...problems(schemas.id, user, "user")]);
      return client.revokeGrant(id, user);
    },
    openVerificationLink: () => client.openVerificationLink(),
    // No input: the callback is the UI's own.
    watchEvents: (onUpdate) => client.watchEvents(onUpdate),
    bots: async (before, limit) => {
      checkPage(before, limit);
      return client.bots(before, limit);
    },
    bot: async (id) => client.bot(check(schemas.id, id, "id")),
    modes: async (before, limit) => {
      checkPage(before, limit);
      return client.modes(before, limit);
    },
    createMode: async (request) => client.createMode(check(schemas.createModeRequest, request)),
    createModeWithCommands: async (request) =>
      client.createModeWithCommands(check(schemas.createModeRequest, request)),
    updateMode: async (id, request) => {
      checkAll([
        ...problems(schemas.id, id, "id"),
        ...problems(schemas.updateModeRequest, request),
      ]);
      return client.updateMode(id, request);
    },
    updateModeWithCommands: async (id, request) => {
      checkAll([
        ...problems(schemas.id, id, "id"),
        ...problems(schemas.updateModeRequest, request),
      ]);
      return client.updateModeWithCommands(id, request);
    },
    deleteMode: async (id) => client.deleteMode(check(schemas.id, id, "id")),
    patchBot: async (id, request) => {
      checkAll([...problems(schemas.id, id, "id"), ...problems(schemas.botPatchRequest, request)]);
      return client.patchBot(id, request);
    },
    setBotMode: async (id, request) => {
      checkAll([...problems(schemas.id, id, "id"), ...problems(schemas.botModeRequest, request)]);
      return client.setBotMode(id, request);
    },
    startBot: async (id) => client.startBot(check(schemas.id, id, "id")),
    stopBot: async (id) => client.stopBot(check(schemas.id, id, "id")),
    restartBot: async (id) => client.restartBot(check(schemas.id, id, "id")),
    resetBot: async (id) => client.resetBot(check(schemas.id, id, "id")),
    resumeBot: async (id) => client.resumeBot(check(schemas.id, id, "id")),
    botChat: async (id, before, limit) => {
      checkAll([
        ...problems(schemas.id, id, "id"),
        ...(before === undefined ? [] : problems(schemas.chatCursor, before, "before")),
        ...(limit === undefined ? [] : problems(schemas.limit, limit, "limit")),
      ]);
      return client.botChat(id, before, limit);
    },
    sendChat: async (id, request) => {
      checkAll([...problems(schemas.id, id, "id"), ...problems(schemas.chatSendRequest, request)]);
      return client.sendChat(id, request);
    },
    sendCommand: async (id, request) => {
      checkAll([
        ...problems(schemas.id, id, "id"),
        ...problems(schemas.chatCommandRequest, request),
      ]);
      return client.sendCommand(id, request);
    },
    serverSettings: async (before, limit) => {
      checkAll([
        ...(before === undefined ? [] : problems(schemas.id, before, "before")),
        ...(limit === undefined ? [] : problems(schemas.serverSettingsLimit, limit, "limit")),
      ]);
      return client.serverSettings(before, limit);
    },
    createServerSettings: async (request) =>
      client.createServerSettings(check(schemas.createServerSettingsRequest, request)),
    updateServerSettings: async (id, request) => {
      checkAll([
        ...problems(schemas.id, id, "id"),
        ...problems(schemas.updateServerSettingsRequest, request),
      ]);
      return client.updateServerSettings(id, request);
    },
    deleteServerSettings: async (id) => client.deleteServerSettings(check(schemas.id, id, "id")),
    previewChatFormats: async (request) =>
      client.previewChatFormats(check(schemas.previewChatFormatsRequest, request)),
    audit: async (before, limit, targetKind, targetId) => {
      checkAll([
        ...(before === undefined ? [] : problems(schemas.chatCursor, before, "before")),
        ...(limit === undefined ? [] : problems(schemas.limit, limit, "limit")),
        ...targetProblems(targetKind, targetId),
      ]);
      return client.audit(before, limit, targetKind, targetId);
    },
    agents: async (before, limit) => {
      checkPage(before, limit);
      return client.agents(before, limit);
    },
    disableAgent: async (id) => client.disableAgent(check(schemas.id, id, "id")),
    releaseAgent: async (id) => client.releaseAgent(check(schemas.id, id, "id")),
    userGrants: async (id, before, limit) => {
      checkAll([...problems(schemas.id, id, "id"), ...pageProblems(before, limit)]);
      return client.userGrants(id, before, limit);
    },
    sharedAccounts: async (id, before, limit) => {
      checkAll([...problems(schemas.id, id, "id"), ...pageProblems(before, limit)]);
      return client.sharedAccounts(id, before, limit);
    },
    leaveAccount: async (id) => client.leaveAccount(check(schemas.id, id, "id")),
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

/** What a page's `before` (an ID, when given) and `limit` (a page size, when given) break. */
function pageProblems(before: string | undefined, limit: number | undefined) {
  return [
    ...(before === undefined ? [] : problems(schemas.id, before, "before")),
    ...(limit === undefined ? [] : problems(schemas.limit, limit, "limit")),
  ];
}

/** What an audit page's target breaks: a kind and an ID, both or neither (ADR-0020, S89). */
function targetProblems(kind: string | undefined, id: string | undefined) {
  if (kind === undefined && id === undefined) {
    return [];
  }
  return [
    ...problems(schemas.auditTargetKind, kind, "target_kind"),
    ...problems(schemas.auditTargetId, id, "target_id"),
  ];
}

/** A page's `before` and `limit`; see `pageProblems`. */
function checkPage(before: string | undefined, limit: number | undefined): void {
  checkAll(pageProblems(before, limit));
}
