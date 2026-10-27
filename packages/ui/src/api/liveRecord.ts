// The live record (Plan.md P11.9; ADR-0020, S71, U42, U46, R107–R111): what the timeline and
// the notices show, taken from the current watch's updates as they arrive. Pure: the live
// layer (app/live.ts) calls it and keeps its result in a per-App store, in memory only, so it
// goes at sign-out and at each new generation; a new watch in the same generation only
// leaves a gap.
//
// - **Entries,** per bot, at most `ENTRIES_MAX`, the oldest dropped: a status that changed
//   against the bot's last known one (kind, state, reason, why, since, attempt, restarting,
//   last disconnect; never the agent or the address), a bot's first frame included (R108);
//   deaths, consecutive ones merged with their count; an alert, merged with the status beside
//   it that entered its state and reason (R112); a mode and a desired state that changed. A
//   repeated frame changes nothing.
//   A fill-in from a read only gives a bot it doesn't know its last known state.
// - **Gaps,** app-wide, at most `GAPS_MAX`: each refetch after the generation's first
//   `ready`, as long as the watch was known down before it (its `disconnected` update or its
//   end, R107); only those of at least `GAP_SHOWN_MS` are kept (U46).
// - **Notices** (U42): a death (merged while its line stays), a paused or failed alert, a
//   blocked address (once per streak), a respawn that failed after an Online session; each
//   also a toast. One for the bot whose screen is open is toasted, never kept (R109). A
//   bot's removal drops its entries, its last known state and its notices.
// - **Times:** an entry keeps its own time where it has one (a reported status's `since`,
//   the agent's clock; a death's or an alert's `at`, the server's) and its arrival, which
//   orders the timeline.
import type { BotResponse } from "../generated/BotResponse";
import type { BotStateKind } from "../generated/BotStateKind";
import type { BotStateReason } from "../generated/BotStateReason";
import type { BotStatus } from "../generated/BotStatus";
import type { DesiredState } from "../generated/DesiredState";
import type { EventMessage } from "../generated/EventMessage";
import type { LiveUpdateMessage } from "../generated/LiveUpdateMessage";

/** The most entries kept per bot. */
export const ENTRIES_MAX = 100;
/** The most gaps kept. */
export const GAPS_MAX = 100;
/** The shortest gap shown (U46): the routine reconnect is shorter. */
export const GAP_SHOWN_MS = 5000;
/** The most toasts kept for the toaster to catch up on. */
const TOASTS_MAX = 20;
/** The most notices kept. */
const NOTICES_MAX = 100;

/** What the record knows of a bot: what it compares the next frame with. */
export interface Known {
  readonly name: string;
  readonly status: BotStatus;
  readonly modeId: string;
  readonly desired: DesiredState;
}

/** One entry of a bot's timeline; `arrived` is when it came, in ms since the epoch. */
export type Entry =
  /**
   * Its status changed; `newDisconnect` when it came with a new last disconnect (R111), and
   * `entered` when its state or reason did, or none was known (R112).
   */
  | {
      readonly kind: "status";
      readonly arrived: number;
      readonly at?: string;
      readonly status: BotStatus;
      readonly desired: DesiredState;
      readonly newDisconnect: boolean;
      readonly entered: boolean;
    }
  /** It died, `count` times in a row, the last at `at`. */
  | { readonly kind: "died"; readonly arrived: number; readonly at: string; readonly count: number }
  /** It rests in `state` for `reason`. */
  | {
      readonly kind: "alert";
      readonly arrived: number;
      readonly at: string;
      readonly state: BotStateKind;
      readonly reason: BotStateReason;
    }
  /** Its mode changed. */
  | { readonly kind: "mode"; readonly arrived: number; readonly from: string; readonly to: string }
  /** Someone asked it to start or stop. */
  | { readonly kind: "desired"; readonly arrived: number; readonly desired: DesiredState };

/** A time the live updates were down, `downMs` long, ending when the refetch `arrived`. */
export interface Gap {
  readonly arrived: number;
  readonly downMs: number;
}

/** What a notice is about. */
export type NoticeKind = "died" | "paused" | "failed" | "blocked" | "respawn";

/** What every notice has. `name` is null for a bot never seen. */
interface NoticeOf {
  readonly id: number;
  readonly botId: string;
  readonly name: string | null;
  readonly count: number;
  readonly at: string;
}

/** What a notice is about: a paused or failed one with its reason. */
type NoticeAbout =
  | { readonly kind: Exclude<NoticeKind, "paused" | "failed"> }
  | { readonly kind: "paused" | "failed"; readonly reason: BotStateReason };

/** A notice: Home's line, and, as it came, a toast. */
export type Notice = NoticeOf & NoticeAbout;

/** One bot's part of the record. */
export interface BotRecord {
  readonly known?: Known;
  readonly entries: readonly Entry[];
  /** A blocked streak's notice was given. */
  readonly blocked: boolean;
}

/** The whole record. */
export interface LiveRecord {
  readonly bots: Readonly<Record<string, BotRecord>>;
  readonly gaps: readonly Gap[];
  readonly notices: readonly Notice[];
  /** The latest notices as they came, each with its own ID, for the toaster. */
  readonly toasts: readonly Notice[];
  /** The last ID given. */
  readonly sequence: number;
  /** The generation's first `ready` came. */
  readonly readied: boolean;
  /** When the watch was last known down, while it hasn't come back. */
  readonly downSince: number | null;
}

/** One item of a bot's timeline: an entry, or a gap. */
export type TimelineItem = Entry | ({ readonly kind: "gap" } & Gap);

/** A record of nothing yet. */
export function emptyRecord(): LiveRecord {
  return {
    bots: {},
    gaps: [],
    notices: [],
    toasts: [],
    sequence: 0,
    readied: false,
    downSince: null,
  };
}

/** `record` after the current watch's `update` arrived at `now`, with bot `open`'s screen open. */
export function observe(
  record: LiveRecord,
  update: LiveUpdateMessage,
  now: number,
  open: string | null,
): LiveRecord {
  switch (update.update) {
    case "refetch":
      return refetched(record, now);
    case "disconnected":
      return down(record, now);
    case "event":
      return evented(record, update.event, now, open);
    default:
      return record;
  }
}

/** `record` with the watch known down from `now`, unless it already was. */
export function down(record: LiveRecord, now: number): LiveRecord {
  return record.downSince === null ? { ...record, downSince: now } : record;
}

/** `record` with each of `bots` known, only where the record doesn't know it yet (R108). */
export function known(record: LiveRecord, bots: readonly BotResponse[]): LiveRecord {
  let next = record;
  for (const bot of bots) {
    const kept = next.bots[bot.id];
    if (kept?.known === undefined) {
      next = withBot(next, bot.id, { ...(kept ?? fresh()), known: knownOf(bot) });
    }
  }
  return next;
}

/** `record` without bot `botId`: its entries, its last known state and its notices. */
export function drop(record: LiveRecord, botId: string): LiveRecord {
  if (record.bots[botId] === undefined && !record.notices.some((one) => one.botId === botId)) {
    return record;
  }
  const { [botId]: _gone, ...bots } = record.bots;
  return { ...record, bots, notices: record.notices.filter((one) => one.botId !== botId) };
}

/** `record` without bot `botId`'s notices: its screen was opened. */
export function acknowledge(record: LiveRecord, botId: string): LiveRecord {
  return record.notices.some((one) => one.botId === botId)
    ? { ...record, notices: record.notices.filter((one) => one.botId !== botId) }
    : record;
}

/** `record` without notice `id`. */
export function dismiss(record: LiveRecord, id: number): LiveRecord {
  return { ...record, notices: record.notices.filter((one) => one.id !== id) };
}

/** Bot `botId`'s timeline: its entries and every gap, newest first. */
export function timeline(record: LiveRecord, botId: string): TimelineItem[] {
  const items: TimelineItem[] = [
    ...(record.bots[botId]?.entries ?? []),
    ...record.gaps.map((gap) => ({ kind: "gap" as const, ...gap })),
  ];
  // A stable sort keeps an entry and a gap that arrived together in the order they came.
  return items.reverse().sort((a, b) => b.arrived - a.arrived);
}

/** A refetch at `now`: a gap when it isn't the generation's first `ready`. */
function refetched(record: LiveRecord, now: number): LiveRecord {
  if (!record.readied) {
    return { ...record, readied: true, downSince: null };
  }
  const downMs = record.downSince === null ? 0 : now - record.downSince;
  const gaps =
    downMs >= GAP_SHOWN_MS
      ? bounded([...record.gaps, { arrived: now, downMs }], GAPS_MAX)
      : record.gaps;
  return { ...record, gaps, downSince: null };
}

/** What `event` changes. */
function evented(
  record: LiveRecord,
  event: EventMessage,
  now: number,
  open: string | null,
): LiveRecord {
  const botId = event.bot_id;
  if (botId === undefined) {
    return record;
  }
  switch (event.type) {
    case "bot":
      return event.bot === undefined ? record : framed(record, event.bot, now, open);
    case "bot_removed":
      return drop(record, botId);
    case "bot_died":
      return event.at === undefined ? record : died(record, botId, event.at, now, open);
    case "bot_alert":
      return event.at === undefined || event.state === undefined || event.reason === undefined
        ? record
        : alerted(
            record,
            botId,
            { at: event.at, state: event.state, reason: event.reason },
            now,
            open,
          );
    default:
      return record;
  }
}

/** A `bot` frame: its changes against what the record knew, and its notices. */
function framed(
  record: LiveRecord,
  bot: BotResponse,
  now: number,
  open: string | null,
): LiveRecord {
  const kept = record.bots[bot.id] ?? fresh();
  const before = kept.known;
  const after = knownOf(bot);
  const entries: Entry[] = [];
  if (before !== undefined && before.desired !== after.desired) {
    entries.push({ kind: "desired", arrived: now, desired: after.desired });
  }
  if (before !== undefined && before.modeId !== after.modeId) {
    entries.push({ kind: "mode", arrived: now, from: before.modeId, to: after.modeId });
  }
  if (before === undefined || statusKey(before.status) !== statusKey(after.status)) {
    const latest = kept.entries.at(-1);
    // Only the status that entered the alert's state and reason is the alert's own (R112): a
    // later change in that state, its agent going or coming back, say, is an entry of its own.
    const entered =
      before === undefined ||
      before.status.state !== after.status.state ||
      before.status.reason !== after.status.reason;
    const merged =
      entered &&
      latest?.kind === "alert" &&
      latest.state === after.status.state &&
      latest.reason === after.status.reason;
    if (!merged) {
      entries.push({
        kind: "status",
        arrived: now,
        ...(after.status.kind === "reported" && after.status.since !== undefined
          ? { at: after.status.since }
          : {}),
        status: after.status,
        desired: after.desired,
        newDisconnect:
          after.status.last_disconnect !== undefined &&
          JSON.stringify(after.status.last_disconnect) !==
            JSON.stringify(before?.status.last_disconnect),
        entered,
      });
    }
  }
  // A streak's notice comes at its first backoff, and not again until the streak ends.
  const streak = blockedStreak(after.status);
  const blocks = streak && !kept.blocked && after.status.state === "backoff";
  let next = withBot(record, bot.id, {
    known: after,
    entries: bounded([...kept.entries, ...entries], ENTRIES_MAX),
    blocked: kept.blocked ? streak : blocks,
  });
  const at = new Date(now).toISOString();
  if (blocks) {
    next = noticed(next, { botId: bot.id, name: after.name, kind: "blocked", at }, open);
  }
  // A failed respawn always ends in a backoff. A bot keeps its last disconnect until a later
  // session ends on its own, so an Online bot that stops may still carry an old one.
  if (
    before?.status.kind === "reported" &&
    before.status.state === "online" &&
    after.status.kind === "reported" &&
    after.status.state === "backoff" &&
    after.status.last_disconnect?.kind === "respawn_failed"
  ) {
    next = noticed(next, { botId: bot.id, name: after.name, kind: "respawn", at }, open);
  }
  return next;
}

/** A death: an entry, merged with the one before when nothing came between, and a notice. */
function died(
  record: LiveRecord,
  botId: string,
  at: string,
  now: number,
  open: string | null,
): LiveRecord {
  const kept = record.bots[botId] ?? fresh();
  const latest = kept.entries.at(-1);
  const lastGap = record.gaps.at(-1)?.arrived ?? Number.NEGATIVE_INFINITY;
  const entries =
    latest?.kind === "died" && latest.arrived > lastGap
      ? [...kept.entries.slice(0, -1), { ...latest, arrived: now, at, count: latest.count + 1 }]
      : bounded(
          [...kept.entries, { kind: "died" as const, arrived: now, at, count: 1 }],
          ENTRIES_MAX,
        );
  const next = withBot(record, botId, { ...kept, entries });
  return noticed(next, { botId, name: kept.known?.name ?? null, kind: "died", at }, open);
}

/** An alert: an entry, merged into the status it comes beside, and a notice. */
function alerted(
  record: LiveRecord,
  botId: string,
  alert: { at: string; state: BotStateKind; reason: BotStateReason },
  now: number,
  open: string | null,
): LiveRecord {
  const kept = record.bots[botId] ?? fresh();
  const latest = kept.entries.at(-1);
  const besideItsStatus =
    latest?.kind === "status" &&
    latest.entered &&
    latest.status.state === alert.state &&
    latest.status.reason === alert.reason;
  const entry: Entry = { kind: "alert", arrived: now, ...alert };
  const entries = besideItsStatus
    ? [...kept.entries.slice(0, -1), entry]
    : bounded([...kept.entries, entry], ENTRIES_MAX);
  const next = withBot(record, botId, { ...kept, entries });
  const kind = alert.state === "failed" ? "failed" : "paused";
  return noticed(
    next,
    { botId, name: kept.known?.name ?? null, kind, at: alert.at, reason: alert.reason },
    open,
  );
}

/**
 * `record` with a notice of `what`: a toast, and Home's line unless bot `open`'s screen shows
 * it (R109); a death merges into its bot's line while that stays.
 */
function noticed(
  record: LiveRecord,
  what: Omit<NoticeOf, "id" | "count"> & NoticeAbout,
  open: string | null,
): LiveRecord {
  const sequence = record.sequence + 1;
  const toast: Notice = { ...what, id: sequence, count: 1 };
  const toasts = bounded([...record.toasts, toast], TOASTS_MAX);
  if (what.botId === open) {
    return { ...record, sequence, toasts };
  }
  const merged = record.notices.find(
    (one) => what.kind === "died" && one.kind === "died" && one.botId === what.botId,
  );
  const notices =
    merged === undefined
      ? bounded([...record.notices, toast], NOTICES_MAX)
      : record.notices.map((one) =>
          one === merged ? { ...one, count: one.count + 1, at: what.at, name: what.name } : one,
        );
  return { ...record, sequence, toasts, notices };
}

/** Whether `status` is in a streak of its agent refusing the bot's address. */
function blockedStreak(status: BotStatus): boolean {
  const last = status.last_disconnect;
  return (
    status.kind === "reported" &&
    status.state !== "online" &&
    last?.kind === "connect_failed" &&
    last.connect_failure === "blocked"
  );
}

/** What the record compares of a status: never the agent or the address. */
function statusKey(status: BotStatus): string {
  return JSON.stringify([
    status.kind,
    status.state,
    status.reason,
    status.why,
    status.since,
    status.attempt,
    status.restarting,
    status.last_disconnect,
  ]);
}

/** What the record keeps of `bot`. */
function knownOf(bot: BotResponse): Known {
  return {
    name: bot.account.name,
    status: bot.status,
    modeId: bot.mode_id,
    desired: bot.desired_state,
  };
}

/** A bot the record holds nothing of. */
function fresh(): BotRecord {
  return { entries: [], blocked: false };
}

/** `record` with bot `botId`'s part `bot`. */
function withBot(record: LiveRecord, botId: string, bot: BotRecord): LiveRecord {
  return { ...record, bots: { ...record.bots, [botId]: bot } };
}

/** The last `max` of `items`. */
function bounded<T>(items: readonly T[], max: number): T[] {
  return items.slice(Math.max(0, items.length - max));
}
