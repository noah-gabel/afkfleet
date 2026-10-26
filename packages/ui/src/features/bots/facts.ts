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
  MODE_LOADING,
  modeName,
  reasonText,
} from "./texts";

/** One fact: its label and its value. */
export interface Fact {
  readonly term: string;
  readonly value: string;
}

/** Where the facts show: a card only what matters at a glance (U35, U37). */
export type Place = "card" | "screen";

/** A minute, the uptime's tick once it's past its first minute. */
const MINUTE = 60_000;

/** `bot`'s facts at `now`, with `modes` every mode the viewer may see (none while loading). */
export function botFacts(
  bot: BotResponse,
  modes: readonly ModeResponse[] | undefined,
  now: number,
  place: Place,
): Fact[] {
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
  const reason = reasonText(bot);
  if (reason !== null) {
    facts.push({ term: LABEL.reason, value: reason });
  }
  // A card shows how the last session ended only while the bot isn't online.
  if (status.last_disconnect !== undefined && (place === "screen" || uptime === null)) {
    facts.push({ term: LABEL.lastDisconnect, value: disconnectText(status.last_disconnect) });
  }
  if (place === "screen" && bot.agent !== undefined) {
    facts.push({ term: LABEL.agent, value: agentText(bot.agent) });
  }
  return facts;
}

/** How often `bot`'s uptime changes as shown: each second in its first minute, then each minute. */
export function uptimeTick(bot: BotResponse, now: number): number {
  const uptime = uptimeMs(bot, now);
  return uptime !== null && uptime < MINUTE ? 1000 : MINUTE;
}

/** How long `bot` has been online at `now`, or null while it isn't. */
function uptimeMs(bot: BotResponse, now: number): number | null {
  const { status } = bot;
  if (
    status.kind !== "reported" ||
    status.state !== "online" ||
    status.online_since === undefined
  ) {
    return null;
  }
  return Math.max(0, now - Date.parse(status.online_since));
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

/** The name of mode `id` among `modes`. */
export function modeLabel(id: string, modes: readonly ModeResponse[] | undefined): string {
  if (modes === undefined) {
    return MODE_LOADING;
  }
  const mode = modes.find((one) => one.id === id);
  return mode === undefined ? HIDDEN_MODE : modeName(mode);
}
