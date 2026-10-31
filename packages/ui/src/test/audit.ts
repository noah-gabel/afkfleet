// The audit log's test data (ADR-0020, R173): placeholders only (CLAUDE.md).
import type { AuditDetail } from "../generated/AuditDetail";
import type { AuditEntryResponse } from "../generated/AuditEntryResponse";
import type { UserResponse } from "../generated/UserResponse";

/** The placeholder ID number `n`. */
export function auditId(n: number): string {
  return `01900000-0000-7000-8000-00000000${n.toString(16).padStart(4, "0")}`;
}

/** Entry `id`, a user's start of a bot unless `overrides` say otherwise. */
export function entry(id: number, overrides: Partial<AuditEntryResponse> = {}): AuditEntryResponse {
  return {
    id,
    at: "2026-10-13T08:00:00.000Z",
    action: "bot.start",
    outcome: "success",
    metadata: [],
    ...overrides,
  };
}

/** A metadata value that's a text. */
export function text(key: string, value: string): AuditDetail {
  return { key, text: value };
}

/** A metadata value that's a number. */
export function int(key: string, value: number): AuditDetail {
  return { key, int: value };
}

/** A metadata value that's a flag. */
export function flag(key: string, value: boolean): AuditDetail {
  return { key, flag: value };
}

/** Every key of `retention.purge`, with its counts (fleet_core::audit::action's docs). */
export function retentionMetadata(overrides: Partial<Record<string, number>> = {}): AuditDetail[] {
  const count = (key: string, value: number) => int(key, overrides[key] ?? value);
  return [
    text("since", "2026-10-13T07:00:00.000Z"),
    text("chat_before", "2026-09-13T08:00:00.000Z"),
    count("chat_by_age", 12),
    count("chat_per_bot", 5000),
    count("chat_by_size", 3),
    text("sessions_before", "2026-07-13T08:00:00.000Z"),
    count("sessions", 2),
    count("rotations", 4),
    text("audit_before", "2024-10-13T08:00:00.000Z"),
    count("audit_entries", 7),
    count("audit_through", 0),
    count("invites", 1),
    count("enrollments", 5),
    count("phases_failed", 0),
  ];
}

/** User `id` named `username`, as the users' list shows them. */
export function listedUser(id: string, username: string): UserResponse {
  return {
    id,
    username,
    role: "member",
    disabled: false,
    mfa_enrolled: true,
    created_at: "2026-10-13T08:00:00.000Z",
  };
}
