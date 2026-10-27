// The bot screens' texts (Plan.md P11.9; ADR-0020's texts table, U35–U38): what a bot's
// state, its reasons and its last disconnect read as. Every value comes from an `Open<…>`,
// so each switch has a default branch that shows a value this version doesn't know as it
// came. A kick's message is a Minecraft server's text: shown as plain text, as it came.
import type { Entry, Notice } from "../../api/liveRecord";
import type { AccountRestriction } from "../../generated/AccountRestriction";
import type { AgentStatus } from "../../generated/AgentStatus";
import type { BotAgent } from "../../generated/BotAgent";
import type { BotResponse } from "../../generated/BotResponse";
import type { BotStateKind } from "../../generated/BotStateKind";
import type { BotStateReason } from "../../generated/BotStateReason";
import type { BotStatus } from "../../generated/BotStatus";
import type { ConnectFailure } from "../../generated/ConnectFailure";
import type { LastDisconnect } from "../../generated/LastDisconnect";
import type { ModeResponse } from "../../generated/ModeResponse";
import type { SessionServerFailure } from "../../generated/SessionServerFailure";
import type { UnplacedReason } from "../../generated/UnplacedReason";
import { formatDuration } from "../../lib/duration";
import { formatClock, formatClockSeconds, formatDate, sameDay } from "../../lib/time";

/** Home's heading. */
export const BOTS_HEADING = "Bots";
/** Home without a bot, and its one next step. */
export const NO_BOTS = "No bots yet. Every Minecraft account you link gets one.";
export const LINK_AN_ACCOUNT = "Link an account";
/** Home while the bots load. */
export const LOADING_BOTS = "Loading bots…";
/** A bot's screen when its read answers 404. */
export const BOT_NOT_FOUND = "This bot doesn't exist, or you can't see it.";
/** A bot's mode the viewer can't see: another user's private mode (S68). */
export const HIDDEN_MODE = "A mode you can't see";
/** A mode's name while the modes load. */
export const MODE_LOADING = "…";
/** A mode's name when the modes couldn't be read. */
export const MODE_FAILED = "Couldn't load the mode";
/** The link back from a bot's screen. */
export const BACK_HOME = "Home";

/** The facts' labels, on the cards and on a bot's screen. */
export const LABEL = {
  server: "Server",
  mode: "Mode",
  stateSince: "State since",
  uptime: "Uptime",
  attempt: "Reconnect attempt",
  reason: "Reason",
  lastDisconnect: "Last disconnect",
  agent: "Agent",
  notRunning: "Not running",
  paused: "Paused",
  failed: "Failed",
} as const;

/** Home when the bots couldn't be read. */
export function botsFailed(why: string): string {
  return `Couldn't load the bots: ${why}`;
}

/** A card's link, named for a screen reader. */
export function openBot(name: string): string {
  return `Open ${name}`;
}

/** When an unreachable agent was last seen: at a time today, on a date before today. */
export function agentLastSeen(iso: string, now: number): string {
  return sameDay(iso, now)
    ? `Its agent was last seen at ${formatClock(iso)}.`
    : `Its agent was last seen on ${formatDate(iso)} at ${formatClock(iso)}.`;
}

/** The badge's look, one of the Badge's variants. */
export type BadgeVariant = "default" | "secondary" | "outline" | "destructive";

/** A bot's state as its badge shows it. */
export interface StateBadge {
  readonly text: string;
  readonly variant: BadgeVariant;
}

/**
 * A bot's badge: its reported state; a Paused or Failed it rests in while not placed;
 * Starting while its agent hasn't reported it yet; Agent offline while its agent can't be
 * reached; Stopped or Not running while it isn't placed.
 */
export function stateBadge(bot: Pick<BotResponse, "status" | "desired_state">): StateBadge {
  const { status } = bot;
  switch (status.kind) {
    case "reported":
      return reportedBadge(status.state);
    case "awaiting_report":
      return { text: "Starting", variant: "secondary" };
    case "unknown":
      return { text: "Agent offline", variant: "outline" };
    case "unplaced":
      if (status.state === "paused" || status.state === "failed") {
        return reportedBadge(status.state);
      }
      return bot.desired_state === "stopped"
        ? { text: "Stopped", variant: "outline" }
        : { text: "Not running", variant: "outline" };
    default:
      return { text: String(status.kind), variant: "outline" };
  }
}

/** A reported state's badge. */
function reportedBadge(state: BotStateKind | undefined): StateBadge {
  switch (state) {
    case "online":
      return { text: "Online", variant: "default" };
    case "connecting":
      return { text: "Connecting", variant: "secondary" };
    case "awaiting_session":
      return { text: "Signing in", variant: "secondary" };
    case "backoff":
      return { text: "Reconnecting", variant: "secondary" };
    case "stopping":
      return { text: "Stopping", variant: "secondary" };
    case "stopped":
      return { text: "Stopped", variant: "outline" };
    case "paused":
      return { text: "Paused", variant: "secondary" };
    case "failed":
      return { text: "Failed", variant: "destructive" };
    default:
      return { text: String(state), variant: "outline" };
  }
}

/** Why a paused or failed bot rests. */
export function stateReasonText(reason: BotStateReason): string {
  switch (reason) {
    case "duplicate_login":
      return "Someone logged in with its account";
    case "banned":
      return "Banned from this server";
    case "not_whitelisted":
      return "Not on this server's whitelist";
    case "wrong_version":
      return "This server runs another Minecraft version";
    case "account_banned":
      return "Its Minecraft account is banned";
    case "multiplayer_disabled":
      return "Multiplayer is turned off for its account";
    case "auth":
      return "Microsoft refused its sign-in: link the account again";
    case "session_denied":
      return "Its session was refused: link the account again, then Reset";
    case "crash_loop":
      return "It kept crashing";
    default:
      return String(reason);
  }
}

/** Why a bot that should run isn't placed. */
export function unplacedText(why: UnplacedReason): string {
  switch (why) {
    case "no_server":
      return "No server set";
    case "owner_disabled":
      return "Its owner's login is disabled";
    case "pending":
      return "Its account waits for approval";
    case "reauth_required":
      return "Its account must be linked again";
    case "refused":
      return "Its account's link was refused";
    case "offline_not_allowed":
      return "This server doesn't allow offline accounts";
    case "awaiting_agent":
      return "Waiting for an agent with room";
    case "stopped":
      return "Stopped";
    default:
      return String(why);
  }
}

/** Why a paused or failed bot rests, wherever its state rests; null for any other state. */
export function restingText(status: BotStatus): string | null {
  return (status.state === "paused" || status.state === "failed") && status.reason !== undefined
    ? stateReasonText(status.reason)
    : null;
}

/** Why a bot that should run isn't placed; never that it's wanted stopped: its badge says so. */
export function notRunningText(status: BotStatus): string | null {
  return status.kind === "unplaced" && status.why !== undefined && status.why !== "stopped"
    ? unplacedText(status.why)
    : null;
}

/** How a bot's last session ended. */
export function disconnectText(last: LastDisconnect): string {
  switch (last.kind) {
    case "kicked":
      if (last.message !== undefined) {
        return `Kicked: ${last.message}`;
      }
      return last.key === undefined ? "Kicked" : `Kicked (${last.key})`;
    case "connection_closed":
      return "Connection closed";
    case "connect_failed":
      return connectFailedText(last.connect_failure);
    case "auth_rejected":
      return "The server refused its sign-in";
    case "account_restricted":
      return last.restriction === undefined
        ? "Account restricted"
        : `Account restricted: ${restrictionText(last.restriction)}`;
    case "session_server_failed":
      return sessionServerText(last.session_server_failure);
    case "session_crashed":
      return "Its session crashed";
    case "watchdog_timeout":
      return "It stopped responding";
    case "liveness_timeout":
      return "The server stopped answering";
    case "respawn_failed":
      return "It couldn't respawn after dying";
    default:
      return String(last.kind);
  }
}

/** A connect that failed, and why. */
function connectFailedText(failure: ConnectFailure | undefined): string {
  switch (failure) {
    case undefined:
      return "Couldn't connect";
    case "blocked":
      return "Blocked: its agent refuses this address. Ask whoever runs the agent to allow it.";
    case "refused":
      return "Couldn't connect: connection refused";
    case "timed_out":
      return "Couldn't connect: timed out";
    case "unresolvable":
      return "Couldn't connect: address not found";
    case "host_unavailable":
      return "Couldn't connect: host unreachable";
    case "other":
      return "Couldn't connect: network error";
    default:
      return `Couldn't connect: ${String(failure)}`;
  }
}

/** How an account is restricted. */
function restrictionText(restriction: AccountRestriction): string {
  switch (restriction) {
    case "banned":
      return "banned";
    case "multiplayer_disabled":
      return "multiplayer turned off";
    default:
      return String(restriction);
  }
}

/** How Minecraft's session server failed. */
function sessionServerText(failure: SessionServerFailure | undefined): string {
  switch (failure) {
    case "unreachable":
      return "Minecraft's session server was unreachable";
    case "rate_limited":
      return "Minecraft's session server rate-limited it";
    case "timed_out":
      return "Minecraft's session server timed out";
    case "unexpected":
      return "Minecraft's session server answered unexpectedly";
    case undefined:
      return "Minecraft's session server failed";
    default:
      return `Minecraft's session server failed (${String(failure)})`;
  }
}

/** A mode's name, a built-in one marked as such. */
export function modeName(mode: ModeResponse): string {
  return mode.ownership === "built_in" ? `${mode.name} (built in)` : mode.name;
}

/** A bot's agent, for Admins and the Owner. */
export function agentText(agent: BotAgent): string {
  return `${agent.name} · ${agentStatusText(agent.status)}`;
}

/** An agent's status. */
function agentStatusText(status: AgentStatus): string {
  switch (status) {
    case "online":
      return "online";
    case "offline":
      return "offline";
    case "expired":
      return "certificate expired";
    case "disabled":
      return "disabled";
    default:
      return String(status);
  }
}

/** The controls' labels (U38). */
export const CONTROL = {
  start: "Start",
  stop: "Stop",
  restart: "Restart",
  resume: "Resume",
  reset: "Reset",
} as const;
/** After a call its bot's agent was sent (S73). */
export const SENT_NOTE = "Sent. The state changes when its agent reports back.";
/** Where the controls would be, for a viewer whose level allows none (U38). */
export const VIEW_ONLY = "You can view this bot but not control it.";

/** Settings (U40, U41, S68, S69). */
export const SETTINGS = {
  heading: "Settings",
  mode: "Mode",
  switching: "Switching…",
  modeForbidden: "Only someone who manages this bot can choose that mode.",
  server: "Server",
  change: "Change",
  set: "Set",
  notSet: "Not set",
  address: "Server address",
  addressHint: "Host or host:port, e.g. play.example.com",
  save: "Save",
  cancel: "Cancel",
} as const;

/** The timeline (U46). */
export const TIMELINE = {
  heading: "Timeline",
  note: "Since you signed in, while the app is open.",
  empty: "Nothing yet.",
} as const;
/** The chat console and its send box (U43–U45, S7, S9, S70; H3's texts). */
export const CHAT = {
  heading: "Chat",
  older: "Show older",
  noOlder: "No older lines.",
  empty: "No chat yet.",
  loading: "Loading chat…",
  server: "Server",
  you: "You",
  aUser: "a user",
  modeTag: "mode",
  argumentsHidden: "(arguments hidden)",
  cutShort: "(cut short)",
  pending: "pending",
  sent: "sent",
  notSent: "not sent",
  placeholder: "Message or /command",
  send: "Send",
  sending: "Sending…",
  commandStepUp: "Enter your password to send this command. It counts for 5 minutes.",
} as const;

/** The chat's history that couldn't be read. */
export function chatFailed(why: string): string {
  return `Couldn't load the chat: ${why}`;
}

/** An older page of the chat that couldn't be read. */
export function olderFailed(why: string): string {
  return `Couldn't load older lines: ${why}`;
}

/** A notice's way to its bot's screen. */
export const OPEN = "Open";
/** A bot the app never saw, where a notice names it. */
export const UNKNOWN_BOT = "A bot";

/**
 * A timeline entry's text, the modes named by `modeNamed` (U46, R111): a status as its badge,
 * with why it doesn't run and a new last disconnect after it.
 */
export function entryText(entry: Entry, modeNamed: (id: string) => string): string {
  switch (entry.kind) {
    case "status": {
      const { status } = entry;
      const last = entry.newDisconnect ? status.last_disconnect : undefined;
      return [
        stateBadge({ status, desired_state: entry.desired }).text,
        notRunningText(status),
        restingText(status),
        last === undefined ? null : disconnectText(last),
      ]
        .filter((part) => part !== null)
        .join(" · ");
    }
    case "died":
      return entry.count === 1
        ? "Died"
        : `Died ${entry.count}×, last at ${formatClockSeconds(entry.at)}`;
    case "alert":
      return `${entry.state === "failed" ? "Failed" : "Paused"}: ${stateReasonText(entry.reason)}`;
    case "mode":
      return `Mode: ${modeNamed(entry.from)} → ${modeNamed(entry.to)}`;
    case "desired":
      return entry.desired === "stopped" ? "Stop requested" : "Start requested";
    default: {
      const unknown: never = entry;
      return String(unknown);
    }
  }
}

/** A gap's text: how long live updates were down. */
export function gapText(downMs: number): string {
  return `Live updates were down for ${formatDuration(downMs)}; some events may be missing.`;
}

/** A notice's toast, as the event happens. */
export function noticeToast(notice: Notice): string {
  return notice.kind === "died" ? `${nameOf(notice)} died.` : noticeLine(notice);
}

/** A notice's line on Home, until it's dismissed or its bot's screen opened. */
export function noticeLine(notice: Notice): string {
  const name = nameOf(notice);
  switch (notice.kind) {
    case "died":
      return notice.count === 1
        ? `${name} died at ${formatClock(notice.at)}.`
        : `${name} died ${notice.count} times, last at ${formatClock(notice.at)}.`;
    case "paused":
      return `${name} was paused: ${stateReasonText(notice.reason)}.`;
    case "failed":
      return `${name} failed: ${stateReasonText(notice.reason)}.`;
    case "blocked":
      return `${name} can't connect: its agent refuses this server's address.`;
    case "respawn":
      return `${name} couldn't respawn and left the server. It reconnects by itself.`;
    default: {
      const unknown: never = notice;
      return String(unknown);
    }
  }
}

/** The bot a notice names, or "A bot" for one never seen. */
function nameOf(notice: Notice): string {
  return notice.name ?? UNKNOWN_BOT;
}
