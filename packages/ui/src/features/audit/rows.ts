// The audit log's rows (Plan.md P11.9; ADR-0020, S52, S85, R173, R174): pages merged by ID, each
// unreadable entry in its place, and an entry's metadata as lines of plain text. A value may come
// as a text, a number or a flag, and a text may be what someone typed or a server reported, so
// nothing here reads meaning into a value's shape but the API's exact time; only the three keys
// the server writes a user's ID under are ever a user (S85).
import type { AuditDetail } from "../../generated/AuditDetail";
import type { AuditEntryResponse } from "../../generated/AuditEntryResponse";
import type { AuditPage } from "../../generated/AuditPage";
import { formatTime } from "../../lib/time";
import {
  connectFailureLabel,
  purged,
  purgedSince,
  purgedThrough,
  purgeKept,
  purgePhasesFailed,
} from "./texts";

/** One row of the log: an entry, or the ID of one that doesn't read back. */
export type AuditRow =
  | { readonly kind: "entry"; readonly id: number; readonly entry: AuditEntryResponse }
  | { readonly kind: "unreadable"; readonly id: number };

/** One line of an entry's details. */
export interface DetailLine {
  /** Its key, with spaces for underscores; empty for a line that says it all. */
  readonly label: string;
  readonly value: string;
  /** The user `value` names, for a key that holds a user's ID. */
  readonly user?: string;
}

/** The keys whose text is a user's ID (fleet_core::audit::action's docs). */
const USER_KEYS: ReadonlySet<string> = new Set(["grantee", "owner", "user_id"]);

/** The keys `retention.purge`'s own lines say. */
const PURGE_KEYS: ReadonlySet<string> = new Set([
  "since",
  "chat_before",
  "chat_per_bot",
  "chat_by_age",
  "chat_by_size",
  "sessions_before",
  "sessions",
  "rotations",
  "audit_before",
  "audit_entries",
  "audit_through",
  "invites",
  "enrollments",
  "phases_failed",
]);

/** The API's times: UTC with milliseconds. */
const TIME = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/;

/** `pages` as one list, newest first; an entry a refetch moved between pages shows once. */
export function auditRows(pages: readonly AuditPage[]): AuditRow[] {
  const rows = new Map<number, AuditRow>();
  for (const page of pages) {
    for (const entry of page.items) {
      rows.set(entry.id, { kind: "entry", id: entry.id, entry });
    }
    for (const id of page.unreadable) {
      rows.set(id, { kind: "unreadable", id });
    }
  }
  return [...rows.values()].sort((a, b) => b.id - a.id);
}

/** `text` as a local time if it's exactly one of the API's that exists, otherwise as stored. */
function shownText(text: string): string {
  return TIME.test(text) && !Number.isNaN(Date.parse(text)) ? formatTime(text) : text;
}

/** How a metadata value reads. */
function shownValue(detail: AuditDetail): string {
  if (detail.text !== undefined) {
    return shownText(detail.text);
  }
  if (detail.int !== undefined) {
    return String(detail.int);
  }
  if (detail.flag !== undefined) {
    return detail.flag ? "yes" : "no";
  }
  return "";
}

/** A metadata value as one line under its key. */
function plainLine(detail: AuditDetail): DetailLine {
  return { label: detail.key.replaceAll("_", " "), value: shownValue(detail) };
}

/** `retention.purge`'s lines; a count only an older server left out shows as a mark. */
function purgeLines(metadata: readonly AuditDetail[]): DetailLine[] {
  const value = (key: string) => {
    const found = metadata.find((detail) => detail.key === key);
    return found === undefined ? "?" : shownValue(found);
  };
  const lines = [
    purged({
      age: value("chat_by_age"),
      size: value("chat_by_size"),
      sessions: value("sessions"),
      rotations: value("rotations"),
      audit: value("audit_entries"),
      invites: value("invites"),
      enrollments: value("enrollments"),
    }),
    purgedSince(value("since")),
    purgeKept(
      value("chat_before"),
      value("sessions_before"),
      value("audit_before"),
      value("chat_per_bot"),
    ),
  ];
  const through = metadata.find((detail) => detail.key === "audit_through")?.int ?? 0;
  if (through > 0) {
    lines.push(purgedThrough(String(through)));
  }
  const failed = metadata.find((detail) => detail.key === "phases_failed")?.int ?? 0;
  if (failed > 0) {
    lines.push(purgePhasesFailed(failed));
  }
  return [
    ...lines.map((text) => ({ label: "", value: text })),
    ...metadata.filter((detail) => !PURGE_KEYS.has(detail.key)).map(plainLine),
  ];
}

/** `entry`'s details, as lines of plain text. */
export function detailLines(entry: AuditEntryResponse): DetailLine[] {
  if (entry.action === "retention.purge") {
    return purgeLines(entry.metadata);
  }
  return entry.metadata.map((detail) => {
    const line = plainLine(detail);
    if (detail.text === undefined) {
      return line;
    }
    if (USER_KEYS.has(detail.key)) {
      return { ...line, value: detail.text, user: detail.text };
    }
    if (
      entry.action === "agent.connect" &&
      entry.outcome === "failure" &&
      detail.key === "reason"
    ) {
      return { ...line, value: connectFailureLabel(detail.text) };
    }
    return line;
  });
}
