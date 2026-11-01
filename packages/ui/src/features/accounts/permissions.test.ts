// The account screens' mirror of `authorize()` against fleet-core's own authorization
// matrix: the snapshot its tests write, read through test/authzMatrix.ts as the admin and bot
// screens' tests read it, so a rule change on the server fails this test until
// permissions.ts follows.
import { describe, expect, it } from "vitest";

import type { AccountKind } from "../../generated/AccountKind";
import type { AccountStatus } from "../../generated/AccountStatus";
import type { GrantLevel } from "../../generated/GrantLevel";
import { checkGrantTable } from "../../test/authzMatrix";
import { accountActions, manages } from "./permissions";

/** A Microsoft account in `status`, which the viewer sees at `level`. */
function microsoft(level: GrantLevel, status: AccountStatus = "active") {
  return { kind: "microsoft" as AccountKind, status, level, leavable: false };
}

describe("account permissions", () => {
  it("offers sharing exactly where the server lets the viewer manage and give grants", () => {
    checkGrantTable("### ManageGrants", (level) => accountActions(microsoft(level)).share);
    for (const level of ["view", "control", "manage"]) {
      checkGrantTable(`### Grant ${level} to another user`, manages);
    }
  });

  it("offers linking again and deleting exactly where the server allows them", () => {
    checkGrantTable("### RelinkAccount", (level) => accountActions(microsoft(level)).linkAgain);
    checkGrantTable("### DeleteAccount", (level) => accountActions(microsoft(level)).delete);
  });

  it("offers linking again only for a Microsoft account that isn't waiting for approval", () => {
    const again = (kind: AccountKind, status: AccountStatus) =>
      accountActions({ kind, status, level: "manage", leavable: false }).linkAgain;

    expect(again("microsoft", "active")).toBe(true);
    expect(again("microsoft", "reauth_required")).toBe(true);
    expect(again("microsoft", "refused")).toBe(true);
    expect(again("microsoft", "pending")).toBe(false);
    expect(again("offline", "active")).toBe(false);
    expect(again("microsoft", "brand_new" as AccountStatus)).toBe(false);
  });

  it("offers leaving exactly as the server says, whatever the level (U83)", () => {
    // The server's own test walks the matrix's `### LeaveAccount` (`leavable_follows_every_cell_
    // of_the_matrix`, R172); here the app must only pass its answer on.
    for (const level of ["view", "control", "manage"] as const) {
      expect(accountActions({ ...microsoft(level), leavable: true }).leave).toBe(true);
      expect(accountActions({ ...microsoft(level), leavable: false }).leave).toBe(false);
    }
  });

  it("gives an unknown level nothing", () => {
    const unknown = "owner" as GrantLevel;

    expect(manages(unknown)).toBe(false);
    expect(accountActions(microsoft(unknown))).toEqual({
      share: false,
      linkAgain: false,
      delete: false,
      leave: false,
    });
  });
});
