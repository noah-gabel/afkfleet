// A chat line as the console shows it (Plan.md P11.9; ADR-0020, U3, U43, U44, U49, S7, S9,
// S70; the times are the user's change at H3's plan review). Every mark sits outside the
// text: the text is what the server sent, plain, and nothing here is ever written into it.
//
// - **The sender** is what a player packet named, never parsed from the text here (S66): a
//   player's name as sent; "Server" for the server's own lines; a mode's line under the
//   bot's name with a "mode" tag; a user's line as asked of the bot, never as said by it
//   (S7), by "You", the sender's name where the API gives it (U3), or "a user". A system
//   line the server's chat format read shows that name with its own mark (U55, S79).
// - **What was left out:** a command's hidden arguments (S9) and a stored text cut short.
// - **A sent line's mark** (S70): pending, sent, or not sent with why when given.
// - **The time** reads HH:MM today and with its date before, the seconds in its tooltip: U49
//   was decided on an HH:MM column, so a line that looks earlier than the one above shows
//   only across a minute, and it gets no mark.
import type { ChatMark } from "../../api/chatOutcomes";
import type { ChatFailure } from "../../generated/ChatFailure";
import type { ChatLine } from "../../generated/ChatLine";
import { formatClock, formatClockSeconds, formatDate, sameDay } from "../../lib/time";
import { CHAT } from "./texts";

/**
 * How a line's sender looks: a player, the server, a name a chat format read from the server's
 * text, the bot's mode, a user, or no one.
 */
export type SenderLook = "player" | "server" | "parsed" | "mode" | "user" | "none";

/** Who a line shows as its sender; for a user's line, what a screen reader reads first (S7). */
export interface Sender {
  readonly label: string | null;
  readonly look: SenderLook;
  readonly asked?: string;
}

/** `line`'s sender, on the bot named `bot`. */
export function senderOf(line: ChatLine, bot: string): Sender {
  const name = line.sender?.name;
  switch (line.kind) {
    case "system":
      // A name a chat format read from the text: display only, marked as such (U55).
      return line.parsed_sender === undefined
        ? { label: CHAT.server, look: "server" }
        : { label: line.parsed_sender, look: "parsed" };
    case "announcement":
      return { label: CHAT.server, look: "server" };
    case "emote":
      return name === undefined ? noOne() : { label: `* ${name}`, look: "player" };
    case "whisper":
      return name === undefined ? noOne() : { label: `${name} (whisper)`, look: "player" };
    case "mode":
      return { label: bot, look: "mode" };
    case "user": {
      const who = line.mine ? CHAT.you : (line.sent_by?.username ?? CHAT.aUser);
      return { label: who, look: "user", asked: `${who} asked ${bot} to send:` };
    }
    default:
      // A player's chat, or a kind this version doesn't know: the name it came with.
      return name === undefined ? noOne() : { label: name, look: "player" };
  }
}

/** No sender shown. */
function noOne(): Sender {
  return { label: null, look: "none" };
}

/** What was left out of `line`'s text, outside it. */
export function lineNotes(line: ChatLine): string[] {
  return [
    ...(line.arguments_hidden ? [CHAT.argumentsHidden] : []),
    ...(line.truncated ? [CHAT.cutShort] : []),
  ];
}

/** A sent line's mark. */
export function markText(mark: ChatMark): string {
  switch (mark.kind) {
    case "pending":
      return CHAT.pending;
    case "sent":
      return CHAT.sent;
    case "failed": {
      const why = mark.failure === undefined ? undefined : failureText(mark.failure);
      return why === undefined ? CHAT.notSent : `${CHAT.notSent}: ${why}`;
    }
    default: {
      const unknown: never = mark;
      return String(unknown);
    }
  }
}

/** Why a line wasn't sent, or nothing for a reason this version doesn't know. */
function failureText(failure: ChatFailure): string | undefined {
  switch (failure) {
    case "bot_not_online":
      return "bot offline";
    case "chat_unavailable":
      return "needs signed chat";
    case "rate_limited":
      return "too fast";
    case "bot_unreachable":
      return "bot unreachable";
    default:
      return undefined;
  }
}

/** `iso` as a line's time at `now`: shown, and in full, to the second, for the tooltip. */
export function chatTime(iso: string, now: number): { shown: string; full: string } {
  return {
    shown: sameDay(iso, now) ? formatClock(iso) : `${formatDate(iso)}, ${formatClock(iso)}`,
    full: `${formatDate(iso)}, ${formatClockSeconds(iso)}`,
  };
}
