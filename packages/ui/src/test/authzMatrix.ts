// fleet-core's authorization matrix, as its tests write it (crates/fleet-core/src/authz/
// snapshots), read as it is by the permission mirrors' tests (accounts, admin, bots), so a
// rule change on the server fails them until the mirror follows (ADR-0017; ADR-0020, S67).
import { expect } from "vitest";

import matrix from "../../../../crates/fleet-core/src/authz/snapshots/fleet_core__authz__authorize__tests__authorization_matrix.snap?raw";
import type { GrantLevel } from "../generated/GrantLevel";
import type { UserRole } from "../generated/UserRole";

/** A table of the snapshot: its header's columns and its rows' cells. */
export interface Table {
  readonly header: readonly string[];
  readonly rows: readonly (readonly string[])[];
}

/** The table that follows the heading `heading` (and its description and blank line). */
export function table(heading: string): Table {
  const lines = matrix.split(/\r?\n/);
  const start = lines.indexOf(heading);
  expect(start, `the snapshot has no "${heading}"`).toBeGreaterThanOrEqual(0);
  const cells = (line: string) => line.trim().split(/\s{2,}/);
  let at = start + 1;
  while (at < lines.length && !/\s{2,}/.test(lines[at] ?? "")) {
    at += 1;
  }
  const header = cells(lines[at] ?? "");
  const rows: string[][] = [];
  for (at += 1; at < lines.length && (lines[at] ?? "").trim() !== ""; at += 1) {
    rows.push(cells(lines[at] ?? ""));
  }
  return { header, rows };
}

/** The roles the matrix names, as the API writes them. */
export function role(label: string): UserRole {
  switch (label) {
    case "Owner":
    case "the Owner":
    case "an Owner":
      return "owner";
    case "Admin":
    case "an Admin":
    case "another Admin":
      return "admin";
    case "Member":
    case "a Member":
    case "another Member":
      return "member";
    default:
      throw new Error(`a role the test doesn't know: ${label}`);
  }
}

/** The grant columns of an account's or a bot's table. */
export const GRANTS = ["no grant", "view", "control", "manage"] as const;

/**
 * The level the server reports to `actor` on an account whose owner the row names, with
 * `grant`: implicit Manage (the owner, the Owner, an Admin on a Member's) or the grant;
 * `null` when the actor can't see it.
 */
export function levelSeen(
  actor: UserRole,
  owner: string,
  grant: (typeof GRANTS)[number],
): GrantLevel | null {
  const implicit =
    owner === "self" || actor === "owner" || (actor === "admin" && role(owner) === "member");
  if (implicit) {
    return "manage";
  }
  return grant === "no grant" ? null : grant;
}

/**
 * Checks every cell of the account or bot table under `heading` against what `allowed` says
 * for the level its viewer would see: "404" where they can't see it, else "ok" or "403".
 */
export function checkGrantTable(heading: string, allowed: (level: GrantLevel) => boolean): void {
  const { header, rows } = table(heading);
  expect(header).toEqual(["actor", "owner", ...GRANTS]);
  expect(rows).toHaveLength(11);
  for (const [actorLabel = "", owner = "", ...cells] of rows) {
    const actor = role(actorLabel);
    GRANTS.forEach((grant, column) => {
      const level = levelSeen(actor, owner, grant);
      const expected = level === null ? "404" : allowed(level) ? "ok" : "403";
      expect(cells[column], `${heading}: ${actorLabel} on ${owner}'s, ${grant}`).toBe(expected);
    });
  }
}
