// What the live events' tests push and seed (Plan.md P11.8): bots, chat lines, modes and the
// updates that carry them. Placeholders only (CLAUDE.md).
import type { InfiniteData } from "@tanstack/react-query";

import type { AccountResponse } from "../generated/AccountResponse";
import type { BotResponse } from "../generated/BotResponse";
import type { ChatLine } from "../generated/ChatLine";
import type { CommandErrorCode } from "../generated/CommandErrorCode";
import type { EventMessage } from "../generated/EventMessage";
import type { LiveUpdateMessage } from "../generated/LiveUpdateMessage";
import type { ModeResponse } from "../generated/ModeResponse";
import { account } from "./accounts";

/** Bot `n`'s ID: a larger `n` is a newer bot. */
export function botId(n: number): string {
  return `01900000-0000-7000-8000-0000000002${String(n).padStart(2, "0")}`;
}

/** Bot `n`, of account `n`, stopped. */
export function bot(n: number, changes: Partial<BotResponse> = {}): BotResponse {
  return {
    id: botId(n),
    account: account(n),
    mode_id: modeId(1),
    desired_state: "stopped",
    status: { kind: "reported", state: "stopped" },
    ...changes,
  };
}

/** Mode `n`'s ID: a larger `n` is a newer mode. */
export function modeId(n: number): string {
  return `01900000-0000-7000-8000-0000000003${String(n).padStart(2, "0")}`;
}

/** Mode `n`, the viewer's own. */
export function mode(n: number, changes: Partial<ModeResponse> = {}): ModeResponse {
  return {
    id: modeId(n),
    name: `Mode ${n}`,
    visibility: "private",
    ownership: "mine",
    editable: true,
    deletable: true,
    visibility_editable: true,
    version: 1,
    created_at: "2026-10-15T10:00:00.000Z",
    updated_at: "2026-10-15T10:00:00.000Z",
    steps: [],
    ...changes,
  };
}

/** Chat line `id`, from the bot's server. */
export function line(id: number, text = `line ${id}`): ChatLine {
  return {
    id,
    at: "2026-10-15T10:00:00.000Z",
    direction: "in",
    kind: "chat",
    text,
    truncated: false,
    arguments_hidden: false,
    mine: false,
  };
}

/** The pages of a list, as an infinite query holds them; `N` is its cursor's type. */
export function pages<T, N extends string | number = string>(
  ...list: { items: T[]; next?: N }[]
): InfiniteData<{ items: T[]; next?: N }, N | undefined> {
  return {
    pages: list,
    pageParams: list.map((_, index) => (index === 0 ? undefined : list[index - 1]?.next)),
  };
}

/** An update carrying `event`. */
export function event(event: EventMessage): LiveUpdateMessage {
  return { update: "event", event };
}

/** A `bot` frame of `bot`. */
export function botFrame(bot: BotResponse): LiveUpdateMessage {
  return event({ type: "bot", bot_id: bot.id, bot });
}

/** A `bot` frame whose account is `account`. */
export function accountFrame(n: number, changes: Partial<AccountResponse>): LiveUpdateMessage {
  return botFrame(bot(n, { account: account(n, changes) }));
}

/** A refetch for `reason`. */
export function refetch(reason: "ready" | "resync" | "unreadable" = "ready"): LiveUpdateMessage {
  return { update: "refetch", reason };
}

/** A disconnect that waits `secs` before the next connect. */
export function disconnected(secs: number): LiveUpdateMessage {
  return { update: "disconnected", disconnect: { why: "lost" }, retry_in_secs: secs };
}

/** A disconnect whose connect failed with `code`. */
export function notOpened(code: CommandErrorCode): LiveUpdateMessage {
  return {
    update: "disconnected",
    disconnect: { why: "not_opened", error: { code, message: `fake ${code}` } },
    retry_in_secs: 2,
  };
}
