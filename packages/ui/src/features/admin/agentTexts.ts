// The agents' screen's texts (Plan.md P11.9; ADR-0020's table). A status is open, so one only a
// newer server sends shows as it was sent.
import type { AgentStatus } from "../../generated/AgentStatus";

export const AGENTS_HEADING = "Agents";
export const AGENTS_INTRO =
  "To add an agent, run afkfleet-server agent create-enrollment-token on the server's host, and pipe the bundle into afkfleet-agent enroll on the agent's host.";
export const NO_AGENTS = "No agent has enrolled yet.";
export const AGENT_COLUMNS = [
  "Name",
  "Status",
  "Bots",
  "Can run",
  "Version",
  "Last seen",
  "Certificate until",
  "Enrolled",
] as const;
export const NEVER_SEEN = "Never";
export const EXPIRED_HINT = "It must enroll again before it can connect.";
export const DISABLE = "Disable";
export const DISABLE_FOR_GOOD = "Disable for good";
export const RELEASE_BOTS = "Release bots";
export const AUDIT_LINK = "Audit";

/** Why the agents couldn't be read. */
export function agentsFailed(why: string): string {
  return `Couldn't load the agents: ${why}`;
}

/** How `status` reads. */
export function agentStatusLabel(status: AgentStatus): string {
  switch (status) {
    case "online":
      return "Online";
    case "offline":
      return "Offline";
    case "expired":
      return "Certificate expired";
    case "disabled":
      return "Disabled";
    default:
      return status;
  }
}

/** A row's "Audit", for screen readers. */
export function auditOf(name: string): string {
  return `Audit of ${name}`;
}

/** What asking before disabling says. */
export function disableQuestion(name: string, bots: number): string {
  const final = `Disabling ${name} is final: its certificates are revoked, it disconnects, and it can't come back. To use that machine again, enroll a new agent.`;
  return bots > 0 ? `${final} Its bots stay with it until you release them.` : final;
}

/** The toast once it's disabled. */
export function disabled(name: string): string {
  return `${name} is disabled.`;
}

/** The line on a disabled agent that holds bots. */
export function botsWait(bots: number): string {
  return bots === 1 ? "1 bot waits here until released." : `${bots} bots wait here until released.`;
}

/** What asking before releasing says: the operator's word. */
export function releaseQuestion(name: string, bots: number): string {
  return `Release ${bots === 1 ? "1 bot" : `${bots} bots`} held by ${name}? Do this only once its host and its bots are stopped: you are vouching for that. A bot still running there could put the same account online twice.`;
}

/** The toast once they're released. */
export function released(name: string): string {
  return `${name}'s bots are released.`;
}
