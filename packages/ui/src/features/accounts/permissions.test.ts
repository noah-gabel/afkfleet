// The account screens' mirror of `authorize()` against fleet-core's own authorization
// matrix: the snapshot its tests write, read here as it is (as the admin screens' test does),
// so a rule change on the server fails this test until permissions.ts follows.
import { describe, expect, it } from "vitest";

import matrix from "../../../../../crates/fleet-core/src/authz/snapshots/fleet_core__authz__authorize__tests__authorization_matrix.snap?raw";
import type { AccountKind } from "../../generated/AccountKind";
import type { AccountStatus } from "../../generated/AccountStatus";
import type { GrantLevel } from "../../generated/GrantLevel";
import type { UserRole } from "../../generated/UserRole";
import { accountActions, manages } from "./permissions";

/** The rows of the table under `heading`, after checking its header and its size. */
function rows(heading: string): readonly (readonly string[])[] {
  const lines = matrix.split(/\r?\n/);
  const start = lines.indexOf(heading);
  expect(start, `the snapshot has no "${heading}"`).toBeGreaterThanOrEqual(0);
  const cells = (line: string) => line.trim().split(/\s{2,}/);
  expect(cells(lines[start + 1] ?? "")).toEqual([
    "actor",
    "owner",
    "no grant",
    "view",
    "control",
    "manage",
  ]);
  const found: string[][] = [];
  for (let at = start + 2; at < lines.length && (lines[at] ?? "").trim() !== ""; at += 1) {
    found.push(cells(lines[at] ?? ""));
  }
  expect(found).toHaveLength(11);
  return found;
}

/** The role a row names. */
function role(label: string): UserRole {
  switch (label) {
    case "Owner":
    case "the Owner":
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

const GRANTS = ["no grant", "view", "control", "manage"] as const;

/**
 * The level the server reports to `actor` on an account whose owner the row names, with
 * `grant`: implicit Manage (the owner, the Owner, an Admin on a Member's) or the grant;
 * `null` when the actor can't see it.
 */
function levelSeen(actor: UserRole, owner: string, grant: (typeof GRANTS)[number]) {
  const implicit =
    owner === "self" || actor === "owner" || (actor === "admin" && role(owner) === "member");
  if (implicit) {
    return "manage" as GrantLevel;
  }
  return grant === "no grant" ? null : grant;
}

/** Checks every cell of `heading`'s table against what `allowed` says for its level. */
function checkTable(heading: string, allowed: (level: GrantLevel) => boolean) {
  for (const [actorLabel = "", owner = "", ...cells] of rows(heading)) {
    const actor = role(actorLabel);
    GRANTS.forEach((grant, column) => {
      const level = levelSeen(actor, owner, grant);
      const expected = level === null ? "404" : allowed(level) ? "ok" : "403";
      expect(cells[column], `${heading}: ${actorLabel} on ${owner}'s, ${grant}`).toBe(expected);
    });
  }
}

/** A Microsoft account in `status`, which the viewer sees at `level`. */
function microsoft(level: GrantLevel, status: AccountStatus = "active") {
  return { kind: "microsoft" as AccountKind, status, level };
}

describe("account permissions", () => {
  it("offers sharing exactly where the server lets the viewer manage and give grants", () => {
    checkTable("### ManageGrants", (level) => accountActions(microsoft(level)).share);
    for (const level of ["view", "control", "manage"]) {
      checkTable(`### Grant ${level} to another user`, manages);
    }
  });

  it("offers linking again and deleting exactly where the server allows them", () => {
    checkTable("### RelinkAccount", (level) => accountActions(microsoft(level)).linkAgain);
    checkTable("### DeleteAccount", (level) => accountActions(microsoft(level)).delete);
  });

  it("offers linking again only for a Microsoft account that isn't waiting for approval", () => {
    const again = (kind: AccountKind, status: AccountStatus) =>
      accountActions({ kind, status, level: "manage" }).linkAgain;

    expect(again("microsoft", "active")).toBe(true);
    expect(again("microsoft", "reauth_required")).toBe(true);
    expect(again("microsoft", "refused")).toBe(true);
    expect(again("microsoft", "pending")).toBe(false);
    expect(again("offline", "active")).toBe(false);
    expect(again("microsoft", "brand_new" as AccountStatus)).toBe(false);
  });

  it("gives an unknown level nothing", () => {
    const unknown = "owner" as GrantLevel;

    expect(manages(unknown)).toBe(false);
    expect(accountActions(microsoft(unknown))).toEqual({
      share: false,
      linkAgain: false,
      delete: false,
    });
  });
});
