// What an Admin or the Owner may do to whom (Plan.md P8.8, ADR-0010, ADR-0017): a mirror of
// fleet-core's `authorize()` for the admin screens, so each row offers only what the server
// would allow. The server stays authoritative: a refusal anyway shows its message.
// permissions.test.ts reads fleet-core's authorization-matrix snapshot as it is, so a rule
// change on the server fails the UI's test until this follows.
// - Nobody changes the Owner or themselves.
// - The Owner sets Members and Admins to Member or Admin, disables or enables them, and
//   reviews their grants (ADR-0020, S50).
// - An Admin only disables or enables Members.
// - A Member invite is an Admin's or the Owner's to create and revoke, an Admin invite only
//   the Owner's, an Owner invite nobody's.
// - The Minecraft servers' settings are Admins' and the Owner's to read and, after a step-up,
//   to write (ADR-0020, S75): the gate of their screens, under the admin layout's
//   `requireAdmin`.
// - The audit log and the agents are Admins' and the Owner's: reading both, disabling an agent
//   and releasing a disabled one's bots (ADR-0020, R172), under the admin layout's gate. Enrolling
//   an agent is nobody's here: only the server's host creates a token (U76).
// A role this app doesn't know (the server's `UserRole` is open) gets nothing, and nothing
// is offered on one.
import type { AgentResponse } from "../../generated/AgentResponse";
import type { InviteResponse } from "../../generated/InviteResponse";
import type { UserRole } from "../../generated/UserRole";

/** Who acts. */
export interface Actor {
  readonly id: string;
  readonly role: UserRole;
}

/** Whom a change is for. */
export interface Target {
  readonly id: string;
  readonly role: UserRole;
}

/** The changes `actor` may make to a user. */
export interface UserActions {
  /** Each role the user may be set to. */
  readonly setRole: Readonly<Record<"member" | "admin" | "owner", boolean>>;
  /** Disabling or enabling. */
  readonly setDisabled: boolean;
  /** Reviewing the grants the user holds and those on their accounts: the Owner's. */
  readonly reviewGrants: boolean;
}

/** Whether `actor` manages `target`: never themselves or the Owner. */
function manages(actor: Actor, target: Target): boolean {
  if (actor.id === target.id || (target.role !== "member" && target.role !== "admin")) {
    return false;
  }
  switch (actor.role) {
    case "owner":
      return true;
    case "admin":
      return target.role === "member";
    default:
      return false;
  }
}

/** What `actor` may change of `target`. */
export function userActions(actor: Actor, target: Target): UserActions {
  const managed = manages(actor, target);
  const setsRoles = managed && actor.role === "owner";
  return {
    setRole: { member: setsRoles, admin: setsRoles, owner: false },
    setDisabled: managed,
    reviewGrants: setsRoles,
  };
}

/** The roles `role` may invite. */
export function inviteRoles(role: UserRole): readonly ("member" | "admin")[] {
  switch (role) {
    case "owner":
      return ["member", "admin"];
    case "admin":
      return ["member"];
    default:
      return [];
  }
}

/** Whether `role` may revoke an invite for `inviteRole`: as it may create one. */
export function canRevokeRole(role: UserRole, inviteRole: UserRole): boolean {
  return (inviteRoles(role) as readonly UserRole[]).includes(inviteRole);
}

/** What a role may do with the Minecraft servers' settings. */
export interface ServerSettingsAccess {
  /** Reading every server's settings (`ViewServerSettings`). */
  readonly view: boolean;
  /** Creating, editing and deleting them (`ConfigureServerSettings`). */
  readonly configure: boolean;
}

/** What `role` may do with the servers' settings: Admins and the Owner everything. */
export function serverSettingsAccess(role: UserRole): ServerSettingsAccess {
  const admin = role === "admin" || role === "owner";
  return { view: admin, configure: admin };
}

/** What a role may do fleet-wide with the agents and the audit log. */
export interface FleetAccess {
  /** Reading the audit log (`ViewAuditLog`). */
  readonly viewAudit: boolean;
  /** Reading the agents (`ViewAgents`). */
  readonly viewAgents: boolean;
  /** Disabling an agent (`DisableAgent`). */
  readonly disableAgent: boolean;
  /** Releasing a disabled agent's bots (`ReleaseAgent`). */
  readonly releaseAgent: boolean;
}

/** What `role` may do with the agents and the audit log: Admins and the Owner everything. */
export function fleetAccess(role: UserRole): FleetAccess {
  const admin = role === "admin" || role === "owner";
  return { viewAudit: admin, viewAgents: admin, disableAgent: admin, releaseAgent: admin };
}

/** What a role may do to one agent. */
export interface AgentActions {
  /** Disabling it, for good. */
  readonly disable: boolean;
  /** Releasing the bots it holds. */
  readonly release: boolean;
}

/**
 * What `role` may do to `agent`, from `enabled` and `bots`, never the open `status`: an enabled
 * agent can be disabled, and a disabled one holding bots can have them released (R172).
 */
export function agentActions(
  agent: Pick<AgentResponse, "enabled" | "bots">,
  role: UserRole,
): AgentActions {
  const access = fleetAccess(role);
  return {
    disable: access.disableAgent && agent.enabled,
    release: access.releaseAgent && !agent.enabled && agent.bots > 0,
  };
}

/** Whether `role` may revoke `invite` now: only a pending one changes. */
export function canRevoke(role: UserRole, invite: InviteResponse): boolean {
  return invite.state === "pending" && canRevokeRole(role, invite.role);
}
