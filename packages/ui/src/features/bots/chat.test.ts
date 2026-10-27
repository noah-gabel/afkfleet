import { describe, expect, it } from "vitest";

import type { ChatLine } from "../../generated/ChatLine";
import type { ChatLineKind } from "../../generated/ChatLineKind";
import { formatClock, formatClockSeconds, formatDate } from "../../lib/time";
import { line } from "../../test/live";
import { chatTime, lineNotes, markText, senderOf } from "./chat";

const BOT = "AfkBot1";

/** A received line of `kind`, from the player `name` when given. */
function received(kind: ChatLineKind, name?: string): ChatLine {
  return { ...line(1), kind, ...(name === undefined ? {} : { sender: { name } }) };
}

/** A user's line, `mine` or by `username`. */
function sent(changes: Partial<ChatLine>): ChatLine {
  return { ...line(1), direction: "out", kind: "user", ...changes };
}

describe("a chat line's sender (U3, U43, S7)", () => {
  it.each([
    ["chat", "Steve", { label: "Steve", look: "player" }],
    ["emote", "Steve", { label: "* Steve", look: "player" }],
    ["whisper", "Steve", { label: "Steve (whisper)", look: "player" }],
    ["system", undefined, { label: "Server", look: "server" }],
    ["announcement", undefined, { label: "Server", look: "server" }],
    ["chat", undefined, { label: null, look: "none" }],
    ["teleport", "Steve", { label: "Steve", look: "player" }],
    ["teleport", undefined, { label: null, look: "none" }],
  ] as const)("shows a received %s line from %s as its sender", (kind, name, shown) => {
    expect(senderOf(received(kind as ChatLineKind, name), BOT)).toEqual(shown);
  });

  it("shows a mode's line with its tag and the bot's name", () => {
    expect(senderOf({ ...line(1), direction: "out", kind: "mode" }, BOT)).toEqual({
      label: BOT,
      look: "mode",
    });
  });

  it("shows a user's line as asked, by You, its sender's name or a user", () => {
    expect(senderOf(sent({ mine: true }), BOT)).toEqual({
      label: "You",
      look: "user",
      asked: "You asked AfkBot1 to send:",
    });
    expect(senderOf(sent({ sent_by: { id: "u", username: "afkowner" } }), BOT)).toEqual({
      label: "afkowner",
      look: "user",
      asked: "afkowner asked AfkBot1 to send:",
    });
    expect(senderOf(sent({}), BOT)).toEqual({
      label: "a user",
      look: "user",
      asked: "a user asked AfkBot1 to send:",
    });
  });
});

describe("a chat line's marks (S9, S70)", () => {
  it("say what was left out of its text", () => {
    expect(lineNotes({ ...line(1), arguments_hidden: true, truncated: true })).toEqual([
      "(arguments hidden)",
      "(cut short)",
    ]);
    expect(lineNotes(line(1))).toEqual([]);
  });

  it.each([
    [{ kind: "pending" }, "pending"],
    [{ kind: "sent" }, "sent"],
    [{ kind: "failed", failure: "bot_not_online" }, "not sent: bot offline"],
    [{ kind: "failed", failure: "chat_unavailable" }, "not sent: needs signed chat"],
    [{ kind: "failed", failure: "rate_limited" }, "not sent: too fast"],
    [{ kind: "failed", failure: "bot_unreachable" }, "not sent: bot unreachable"],
    [{ kind: "failed", failure: "queue_full" }, "not sent"],
    [{ kind: "failed" }, "not sent"],
  ] as const)("say %o as %s", (mark, text) => {
    expect(markText(mark as Parameters<typeof markText>[0])).toBe(text);
  });
});

describe("a chat line's time (the user's change at H3's plan review)", () => {
  const NOW = Date.parse("2026-10-27T15:00:00.000Z");

  it("shows today's as HH:MM and older ones with their date, the seconds in the tooltip", () => {
    const today = new Date(NOW - 60_000).toISOString();
    const before = new Date(NOW - 3 * 86_400_000).toISOString();

    expect(chatTime(today, NOW)).toEqual({
      shown: formatClock(today),
      full: `${formatDate(today)}, ${formatClockSeconds(today)}`,
    });
    expect(chatTime(before, NOW)).toEqual({
      shown: `${formatDate(before)}, ${formatClock(before)}`,
      full: `${formatDate(before)}, ${formatClockSeconds(before)}`,
    });
  });
});
