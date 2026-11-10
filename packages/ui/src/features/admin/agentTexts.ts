// The agents' screen's texts (Plan.md P11.9; ADR-0020's table). A status is open, so one only a
// newer server sends shows as it was sent.
import type { Tone } from "../../components/StatusLine";
import type { AgentStatus } from "../../generated/AgentStatus";

export const AGENTS_HEADING = "Agents";
export const NO_AGENTS = "No agents.";
export const LOADING_AGENTS = "Loading agents…";
/** The two commands of an enrollment, which the intro names in mono. */
export const ENROLL_COMMANDS = {
  token: "afkfleet-server agent create-enrollment-token",
  enroll: "afkfleet-agent enroll",
} as const;
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
export const EXPIRED_HINT = "Certificate expired. It can't connect; enroll a new agent.";
export const DISABLE = "Disable";
export const DISABLE_FOR_GOOD = "Disable for good";
export const RELEASE_BOTS = "Release bots";
export const AUDIT_LINK = "Audit";

/** Why the agents couldn't be read. */
export function agentsFailed(why: string): string {
  return `Couldn't load agents: ${why}`;
}

/** How `status` reads. */
export function agentStatusLabel(status: AgentStatus): string {
  switch (status) {
    case "online":
      return "Online";
    case "offline":
      return "Offline";
    case "expired":
      return "Expired";
    case "disabled":
      return "Disabled";
    default:
      return status;
  }
}

/** What `status` means at a glance (R229): Expired a warning, Disabled final; unknown, neutral. */
export function agentStatusTone(status: AgentStatus): Tone {
  switch (status) {
    case "online":
      return "success";
    case "expired":
      return "warning";
    case "disabled":
      return "destructive";
    default:
      return "neutral";
  }
}

/** A row's "Audit", for screen readers. */
export function auditOf(name: string): string {
  return `Audit of ${name}`;
}

/** What asking before disabling says. */
export function disableQuestion(name: string, bots: number): string {
  const final = `Disable ${name} for good? Its certificates are revoked; it disconnects and can't return. To use that machine again, enroll a new agent.`;
  return bots > 0 ? `${final} Its bots stay held until you release them.` : final;
}

/** The toast once it's disabled. */
export function disabled(name: string): string {
  return `${name} disabled.`;
}

/** The line on a disabled agent that holds bots. */
export function botsWait(bots: number): string {
  return bots === 1 ? "1 bot held until released." : `${bots} bots held until released.`;
}

/** What asking before releasing says: the operator's word. */
export function releaseQuestion(name: string, bots: number): string {
  return `Release ${bots === 1 ? "1 bot" : `${bots} bots`} held by ${name}? Do this only once its host and its bots are stopped: you are vouching for that. A bot still running there could put the same account online twice.`;
}

/** The toast once they're released. */
export function released(name: string): string {
  return `${name}'s bots released.`;
}

/** How many days ahead an agent's certificate end is warned of (the user's choice, PR #92's review). */
export const CERTIFICATE_WARNING_DAYS = 14;

/** The day `at` falls on here, as a count of days, so a daylight-saving change moves no day. */
function localDay(at: number): number {
  const date = new Date(at);
  return Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()) / 86_400_000;
}

/**
 * The warning on an enabled agent whose certificate ends within 14 calendar days of `now`, here,
 * or none: none once it has ended, which the server shows as Expired. "Certificate until" drops in a
 * small window, so this is where the end shows (the user's words, PR #92's review). An agent renews
 * its certificate with a third of its lifetime left, 20 of 60 days (P13.6, ADR-0027), so one that
 * gets here hasn't: it's offline, or older than 0.1.2, which is what the text says to change.
 */
export function certificateWarning(notAfter: string, now: number): string | undefined {
  const end = Date.parse(notAfter);
  if (Number.isNaN(end) || end <= now) {
    return undefined;
  }
  const days = localDay(end) - localDay(now);
  if (days > CERTIFICATE_WARNING_DAYS) {
    return undefined;
  }
  const when = days === 0 ? "today" : days === 1 ? "tomorrow" : `in ${days} days`;
  return `Expires ${when}. It renews once connected on 0.1.2 or later.`;
}
