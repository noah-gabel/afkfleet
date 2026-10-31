// The audit log's rows (ADR-0020, R173, R174, S52, S85): pages merged by ID with their
// unreadable entries in place, and each entry's details as plain lines.
import { describe, expect, it } from "vitest";

import type { AuditDetail } from "../../generated/AuditDetail";
import { formatTime } from "../../lib/time";
import { auditId, entry, flag, int, retentionMetadata, text } from "../../test/audit";
import { auditRows, detailLines } from "./rows";

describe("auditRows", () => {
  it("puts each unreadable entry in its place by ID, within a page and across pages", () => {
    const rows = auditRows([
      { items: [entry(9), entry(7)], unreadable: [8], next: 7 },
      { items: [entry(5)], unreadable: [6, 4] },
    ]);

    expect(rows.map((row) => [row.id, row.kind])).toEqual([
      [9, "entry"],
      [8, "unreadable"],
      [7, "entry"],
      [6, "unreadable"],
      [5, "entry"],
      [4, "unreadable"],
    ]);
  });

  it("lists an entry a refetch moved between pages once", () => {
    const rows = auditRows([
      { items: [entry(9), entry(8)], unreadable: [] },
      { items: [entry(8), entry(7)], unreadable: [] },
    ]);

    expect(rows.map((row) => row.id)).toEqual([9, 8, 7]);
  });

  it("holds nothing for no pages", () => {
    expect(auditRows([])).toEqual([]);
  });
});

describe("detailLines", () => {
  const lines = (metadata: AuditDetail[], action = "bot.start", outcome = "success") =>
    detailLines(entry(1, { metadata, action, outcome } as Parameters<typeof entry>[1]));

  it("shows a text, a number and a flag under their keys, with spaces for underscores", () => {
    expect(
      lines([
        text("server_host", "mc.example.com"),
        int("sessions_revoked", 2),
        flag("disabled", true),
      ]),
    ).toEqual([
      { label: "server host", value: "mc.example.com" },
      { label: "sessions revoked", value: "2" },
      { label: "disabled", value: "yes" },
    ]);
    expect(lines([flag("rotated", false)])).toEqual([{ label: "rotated", value: "no" }]);
  });

  it("shows a detail without a value as its key alone", () => {
    expect(lines([{ key: "something_new" }])).toEqual([{ label: "something new", value: "" }]);
  });

  it("shows a text in the API's exact time shape as a local time, and nothing else", () => {
    const at = "2026-10-13T08:00:00.000Z";

    expect(lines([text("expires_at", at)])).toEqual([
      { label: "expires at", value: formatTime(at) },
    ]);
    expect(lines([text("name", "2026-10-13"), text("host", "2026-10-13T08:00:00Z")])).toEqual([
      { label: "name", value: "2026-10-13" },
      { label: "host", value: "2026-10-13T08:00:00Z" },
    ]);
    // A row can hold a time that doesn't exist; it shows as stored, never a crash.
    expect(lines([text("since", "2026-13-45T08:00:00.000Z")])).toEqual([
      { label: "since", value: "2026-13-45T08:00:00.000Z" },
    ]);
  });

  it("marks only grantee, owner and user_id as a user, never by a value's shape (S85)", () => {
    const id = auditId(1);

    expect(lines([text("grantee", id), text("owner", id), text("user_id", id)])).toEqual([
      { label: "grantee", value: id, user: id },
      { label: "owner", value: id, user: id },
      { label: "user id", value: id, user: id },
    ]);
    expect(lines([text("name", id), text("server_host", id), text("flow", id)])).toEqual([
      { label: "name", value: id },
      { label: "server host", value: id },
      { label: "flow", value: id },
    ]);
    expect(lines([int("owner", 1)])).toEqual([{ label: "owner", value: "1" }]);
  });

  it("words an agent's connect failure, and only that (R174)", () => {
    expect(lines([text("reason", "san_mismatch")], "agent.connect", "failure")).toEqual([
      { label: "reason", value: "Its certificate names a different agent" },
    ]);
    expect(lines([text("reason", "new_reason")], "agent.connect", "failure")).toEqual([
      { label: "reason", value: "new_reason" },
    ]);
    expect(lines([text("reason", "san_mismatch")], "agent.connect", "success")).toEqual([
      { label: "reason", value: "san_mismatch" },
    ]);
    expect(lines([text("reason", "expired")], "auth.login", "failure")).toEqual([
      { label: "reason", value: "expired" },
    ]);
  });

  describe("of retention.purge", () => {
    const purge = (metadata: AuditDetail[]) =>
      detailLines(entry(1, { action: "retention.purge", metadata }));

    it("says what went, since when and what's kept, in the user's words", () => {
      expect(purge(retentionMetadata())).toEqual([
        {
          label: "",
          value:
            "Chat lines: 12 by age, 3 by size · Sessions: 2 · Replaced login tokens: 4 · Audit entries: 7 · Invites: 1 · Enrollment tokens: 5",
        },
        { label: "", value: `Since ${formatTime("2026-10-13T07:00:00.000Z")}.` },
        {
          label: "",
          value: `Kept: chat after ${formatTime("2026-09-13T08:00:00.000Z")}, sessions after ${formatTime("2026-07-13T08:00:00.000Z")}, the log after ${formatTime("2024-10-13T08:00:00.000Z")}; at most 5000 chat lines per bot.`,
        },
      ]);
    });

    it("adds what the log no longer holds and what failed, only when any", () => {
      const values = purge(retentionMetadata({ audit_through: 41, phases_failed: 2 })).map(
        (line) => line.value,
      );

      expect(values).toContain("The log no longer holds entries up to 41.");
      expect(values).toContain("2 parts failed this time. The server's log says why.");
      expect(purge(retentionMetadata({ phases_failed: 1 })).map((line) => line.value)).toContain(
        "1 part failed this time. The server's log says why.",
      );
    });

    it("lists a key only a newer server records after them, and a missing count as a mark", () => {
      const metadata = retentionMetadata().filter((detail) => detail.key !== "sessions");

      const shown = purge([...metadata, int("new_count", 9)]);

      expect(shown.at(0)?.value).toContain("Sessions: ?");
      expect(shown.at(-1)).toEqual({ label: "new count", value: "9" });
    });
  });
});
