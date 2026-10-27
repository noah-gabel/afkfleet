// What a bot's card and its screen show (Plan.md P11.9; ADR-0020, U37, S67, S68, R93):
// each fact the bot has, labelled, in order; one it doesn't have is left out. The address
// and the agent show when the server sent them, never by a rule of the UI's own (S67). A
// mode's name comes from every mode the viewer may see; a mode missing from that list is
// another user's private one, named as such, never by its ID (S68). An uptime counts from
// the agent's `online_since` on the PC's clock, never below zero (R93).
import type { BotResponse } from "../../generated/BotResponse";
import type { BotStatus } from "../../generated/BotStatus";
import type { ModeResponse } from "../../generated/ModeResponse";
import { formatDuration } from "../../lib/duration";
import { formatTime } from "../../lib/time";
import {
  agentText,
  disconnectText,
  HIDDEN_MODE,
  LABEL,
  MODE_FAILED,
  MODE_LOADING,
  modeName,
  notRunningText,
  restingText,
} from "./texts";

/** One fact: its label and its value. */
export interface Fact {
  readonly term: string;
  readonly value: string;
}

/** Every mode the viewer may see, or that they're still loading or couldn't be read. */
export type Modes = readonly ModeResponse[] | "loading" | "failed";

/** What `modes`, the every-mode query, holds for the facts: its data, else whether it failed. */
export function modesOf(modes: {
  readonly data:
    | { readonly pages: readonly { readonly items: readonly ModeResponse[] }[] }
    | undefined;
  readonly error: unknown;
}): Modes {
  if (modes.data !== undefined) {
    return modes.data.pages.flatMap((page) => page.items);
  }
  return modes.error === null ? "loading" : "failed";
}

/** Where the facts show: a card only what matters at a glance (U35, U37). */
export type Place = "card" | "screen";

/** A second and a minute, the uptime's ticks in its first minute and after it. */
const SECOND = 1000;
const MINUTE = 60_000;

/** `bot`'s facts at `now`, with `modes` every mode the viewer may see (none while loading). */
export function botFacts(bot: BotResponse, modes: Modes, now: number, place: Place): Fact[] {
  const { status } = bot;
  const facts: Fact[] = [];
  if (bot.server_address !== undefined) {
    facts.push({ term: LABEL.server, value: bot.server_address });
  }
  facts.push({ term: LABEL.mode, value: modeLabel(bot.mode_id, modes) });
  if (place === "screen" && status.since !== undefined) {
    facts.push({ term: LABEL.stateSince, value: formatTime(status.since) });
  }
  const uptime = uptimeMs(bot, now);
  if (uptime !== null) {
    facts.push({ term: LABEL.uptime, value: formatDuration(uptime) });
  }
  if (place === "screen" && status.attempt !== undefined && trying(status)) {
    facts.push({ term: LABEL.attempt, value: String(status.attempt) });
  }
  facts.push(...reasons(status));
  // A card shows how the last session ended only while the bot isn't online.
  if (status.last_disconnect !== undefined && (place === "screen" || uptime === null)) {
    facts.push({ term: LABEL.lastDisconnect, value: disconnectText(status.last_disconnect) });
  }
  if (place === "screen" && bot.agent !== undefined) {
    facts.push({ term: LABEL.agent, value: agentText(bot.agent) });
  }
  return facts;
}

/**
 * In how many milliseconds after `now` `bot`'s uptime changes as shown (R100): at its next
 * whole second in its first minute, then at its next whole minute; a minute at most, also
 * while it doesn't count.
 */
export function uptimeNextChange(bot: BotResponse, now: number): number {
  const online = onlineMs(bot, now);
  if (online === null) {
    return MINUTE;
  }
  if (online < 0) {
    // A clock ahead of the PC's: "0 s" until the first second.
    return Math.min(MINUTE, SECOND - online);
  }
  return online < MINUTE ? SECOND - (online % SECOND) : MINUTE - (online % MINUTE);
}

/** How long `bot` has been online at `now`, never below zero, or null while it isn't. */
function uptimeMs(bot: BotResponse, now: number): number | null {
  const online = onlineMs(bot, now);
  return online === null ? null : Math.max(0, online);
}

/** `now` less when `bot` went online, by the agent's clock; null while it isn't online. */
function onlineMs(bot: BotResponse, now: number): number | null {
  const { status } = bot;
  if (
    status.kind !== "reported" ||
    status.state !== "online" ||
    status.online_since === undefined
  ) {
    return null;
  }
  return now - Date.parse(status.online_since);
}

/** Whether a bot is trying to join: backing off, or a later attempt's connect. */
function trying(status: BotStatus): boolean {
  switch (status.state) {
    case "backoff":
      return true;
    case "connecting":
    case "awaiting_session":
      return (status.attempt ?? 0) > 1;
    default:
      return false;
  }
}

/**
 * Why a bot doesn't run: why it isn't placed, why it rests paused or failed, or both, the
 * not-running one first, each under its own label; one alone under "Reason".
 */
function reasons(status: BotStatus): Fact[] {
  const notRunning = notRunningText(status);
  const resting = restingText(status);
  if (notRunning !== null && resting !== null) {
    return [
      { term: LABEL.notRunning, value: notRunning },
      { term: status.state === "failed" ? LABEL.failed : LABEL.paused, value: resting },
    ];
  }
  const reason = notRunning ?? resting;
  return reason === null ? [] : [{ term: LABEL.reason, value: reason }];
}

/** The name of mode `id` among `modes`. */
export function modeLabel(id: string, modes: Modes): string {
  if (modes === "loading") {
    return MODE_LOADING;
  }
  if (modes === "failed") {
    return MODE_FAILED;
  }
  const mode = modes.find((one) => one.id === id);
  return mode === undefined ? HIDDEN_MODE : modeName(mode);
}
