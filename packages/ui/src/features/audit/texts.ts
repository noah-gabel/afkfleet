// The audit log's texts (Plan.md P11.9; ADR-0020's table): each name's word, for an Admin or the
// Owner reading the log. The names are open, so one only a newer server records shows as it was
// sent; a `Map` looks them up, so a name like `constructor` can't reach a prototype's.
import type { AuditActionName } from "../../generated/AuditActionName";
import type { AuditOutcomeName } from "../../generated/AuditOutcomeName";
import type { AuditTargetKind } from "../../generated/AuditTargetKind";

export const AUDIT_HEADING = "Audit log";
export const AUDIT_INTRO = "Everything the server recorded, newest first.";
export const AUDIT_EMPTY = "Nothing recorded yet.";
export const FILTER_EMPTY = "Nothing recorded for this target.";
export const SHOW_MORE = "Show more";
export const LOAD_AGAIN = "Load again";
export const ONLY_THIS_TARGET = "Only this target";
export const SHOW_EVERYTHING = "Show everything";
export const FILTER_ON_USER = "This shows what happened to this user, not everything they did.";
export const NAMES_FAILED = "Names couldn't be loaded, so users show by ID.";
export const THE_SERVER = "The server";
export const NO_USER = "No user";
export const COLUMNS = ["When", "Who", "What", "On", "Result", "From", "Details"] as const;

/** Why the log couldn't be read. */
export function auditFailed(why: string): string {
  return `Couldn't load the audit log: ${why}`;
}

/** What the line above a filtered list says; `who` is a user's name or a target's ID. */
export function showingOnly(kind: AuditTargetKind, who: string): string {
  return `Showing only ${targetKindLabel(kind).toLowerCase()} ${who}.`;
}

/** An entry's "Only this target", for screen readers. */
export function onlyTarget(kind: AuditTargetKind, who: string): string {
  return `Only ${targetKindLabel(kind).toLowerCase()} ${who}`;
}

/** A row the server couldn't read back (ADR-0020, S52). */
export function unreadableEntry(id: number): string {
  return `Entry ${id} can't be read. The server's log says why.`;
}

/** Every action's word; `tsc` refuses a new action without one. */
const ACTIONS: Record<AuditActionName, string> = {
  "user.create": "User created",
  "security.refresh_token_reuse": "Replaced login token used again; session ended",
  "security.lockout": "Username locked after failed logins",
  "mfa.enroll": "2FA set up",
  "mfa.reset": "2FA replaced",
  "auth.login": "Login",
  "auth.logout": "Logout",
  "auth.step_up": "Password confirmed",
  "user.set_role": "Role changed",
  "user.set_disabled": "User disabled or enabled",
  "invite.create": "Invite created",
  "invite.revoke": "Invite revoked",
  "invite.use": "Invite used",
  "user.change_password": "Password changed",
  "session.revoke": "Session ended",
  "vault.rotate_key": "Vault key rotated",
  "account.refresh": "Microsoft token refreshed",
  "account.set_status": "Account status changed",
  "account.rename": "Account renamed",
  "security.account_profile_mismatch": "Account's profile didn't match",
  "account.start_link": "Microsoft link started",
  "account.link": "Account linked",
  "account.approve": "Account approved",
  "account.refuse": "Account refused",
  "account.delete": "Account deleted",
  "account.relink": "Account linked again",
  "grant.create": "Account shared",
  "grant.set_level": "Access changed",
  "grant.revoke": "Access ended",
  "enrollment_token.create": "Enrollment token created",
  "enrollment_token.use": "Enrollment token used",
  "agent.enroll": "Agent enrolled",
  "agent.connect": "Agent connection",
  "agent.disconnect": "Agent disconnected",
  "security.agent_identity_conflict": "Two processes using one agent's identity",
  "agent.disable": "Agent disabled",
  "bot.assign": "Bot placed on an agent",
  "bot.unassign": "Bot taken off an agent",
  "bot.detach": "Agent no longer runs a bot",
  "bot.stray": "Agent reported a bot it wasn't given",
  "agent.release": "Agent's bots released",
  "bot.set_server": "Bot's server set",
  "bot.set_mode": "Bot's mode changed",
  "bot.start": "Bot started",
  "bot.stop": "Bot stopped",
  "bot.restart": "Bot restarted",
  "bot.reset": "Bot reset",
  "bot.resume": "Bot resumed",
  "bot.send_chat": "Bot sent a chat line",
  "bot.send_command": "Bot sent a command",
  "mode.create": "Mode created",
  "mode.update": "Mode edited",
  "mode.delete": "Mode deleted",
  "retention.purge": "Old data deleted",
  "server_settings.create": "Server settings added",
  "server_settings.update": "Server settings changed",
  "server_settings.delete": "Server settings deleted",
};

/** Every target kind's word. */
const TARGET_KINDS: Record<AuditTargetKind, string> = {
  user: "User",
  account: "Account",
  bot: "Bot",
  agent: "Agent",
  mode: "Mode",
  session: "Session",
  invite: "Invite",
  enrollment_token: "Enrollment token",
  server_settings: "Server settings",
};

/** Every outcome's word. */
const OUTCOMES: Record<AuditOutcomeName, string> = {
  success: "Done",
  failure: "Failed",
  denied: "Denied",
};

/** Why an agent's connection was refused, as `agent.connect`'s Failure records it. */
const CONNECT_FAILURES: Record<string, string> = {
  revoked: "Its certificate was revoked",
  expired: "Its certificate expired",
  disabled: "The agent is disabled",
  san_mismatch: "Its certificate names a different agent",
  name_mismatch: "It gave another name than it enrolled with",
};

/** Looks `name` up in `words`; a name it doesn't hold is its own word. */
function worded(words: Record<string, string>): (name: string) => string {
  const known = new Map(Object.entries(words));
  return (name) => known.get(name) ?? name;
}

const actionWord = worded(ACTIONS);
const targetKindWord = worded(TARGET_KINDS);
const outcomeWord = worded(OUTCOMES);
const connectFailureWord = worded(CONNECT_FAILURES);

/** How `action` reads. */
export function actionLabel(action: AuditActionName): string {
  return actionWord(action);
}

/** How `outcome` reads. */
export function outcomeLabel(outcome: AuditOutcomeName): string {
  return outcomeWord(outcome);
}

/** How a target's `kind` reads. */
export function targetKindLabel(kind: AuditTargetKind): string {
  return targetKindWord(kind);
}

/** How `agent.connect`'s `reason` reads. */
export function connectFailureLabel(reason: string): string {
  return connectFailureWord(reason);
}

/** What `retention.purge` deleted. */
export function purged(counts: {
  readonly age: string;
  readonly size: string;
  readonly sessions: string;
  readonly rotations: string;
  readonly audit: string;
  readonly invites: string;
  readonly enrollments: string;
}): string {
  return `Chat lines: ${counts.age} by age, ${counts.size} by size · Sessions: ${counts.sessions} · Replaced login tokens: ${counts.rotations} · Audit entries: ${counts.audit} · Invites: ${counts.invites} · Enrollment tokens: ${counts.enrollments}`;
}

/** Which period `retention.purge` sums up. */
export function purgedSince(time: string): string {
  return `Since ${time}.`;
}

/** What `retention.purge`'s latest run kept. */
export function purgeKept(chat: string, sessions: string, log: string, perBot: string): string {
  return `Kept: chat after ${chat}, sessions after ${sessions}, the log after ${log}; at most ${perBot} chat lines per bot.`;
}

/** The highest entry `retention.purge` deleted. */
export function purgedThrough(id: string): string {
  return `The log no longer holds entries up to ${id}.`;
}

/** How many of `retention.purge`'s phases failed. */
export function purgePhasesFailed(count: number): string {
  return `${count === 1 ? "1 part" : `${count} parts`} failed this time. The server's log says why.`;
}
