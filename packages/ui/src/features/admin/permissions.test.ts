// The admin screens' mirror of `authorize()` against fleet-core's own authorization matrix:
// the snapshot its tests write (crates/fleet-core/src/authz/snapshots), read here as it is,
// so a rule change on the server fails this test until permissions.ts follows, the way
// ipc-contract.json keeps the commands in step.
import { describe, expect, it } from "vitest";

import type { InviteResponse } from "../../generated/InviteResponse";
import type { UserRole } from "../../generated/UserRole";
import { role, table } from "../../test/authzMatrix";
import { privilegeOf } from "../shell/nav";
import {
  type Actor,
  canRevoke,
  canRevokeRole,
  inviteRoles,
  serverSettingsAccess,
  userActions,
} from "./permissions";

const ACTOR_ID = "01900000-0000-7000-8000-000000000001";
const OTHER_ID = "01900000-0000-7000-8000-000000000002";

describe("userActions", () => {
  const users = table("## Users");

  it("reads the matrix's Users table as it is", () => {
    expect(users.header).toEqual([
      "actor",
      "target",
      "SetRole member",
      "SetRole admin",
      "SetRole owner",
      "SetDisabled",
      "ReviewUserGrants",
    ]);
    expect(users.rows).toHaveLength(11);
  });

  it("allows exactly what the matrix allows, for every actor and target", () => {
    for (const [actorLabel = "", targetLabel = "", ...cells] of users.rows) {
      const actor: Actor = { id: ACTOR_ID, role: role(actorLabel) };
      const target = targetLabel === "self" ? actor : { id: OTHER_ID, role: role(targetLabel) };
      const actions = userActions(actor, target);
      const allowed = [
        actions.setRole.member,
        actions.setRole.admin,
        actions.setRole.owner,
        actions.setDisabled,
        actions.reviewGrants,
      ];

      expect(allowed, `${actorLabel} on ${targetLabel}`).toEqual(
        cells.map((cell) => cell === "ok"),
      );
    }
  });

  it("gives a role this app doesn't know nothing, and allows nothing on one", () => {
    const unknown = "superuser" as UserRole;

    const as = userActions({ id: ACTOR_ID, role: unknown }, { id: OTHER_ID, role: "member" });
    const on = userActions({ id: ACTOR_ID, role: "owner" }, { id: OTHER_ID, role: unknown });

    for (const actions of [as, on]) {
      expect(actions).toEqual({
        setRole: { member: false, admin: false, owner: false },
        setDisabled: false,
        reviewGrants: false,
      });
    }
  });
});

describe("inviteRoles", () => {
  const fleet = table("## Fleet-wide");

  it("reads the matrix's invite rows as they are", () => {
    expect(fleet.header).toEqual(["permission", "Owner", "Admin", "Member"]);
    expect(fleet.rows.filter(([name]) => name?.startsWith("CreateInvite for "))).toHaveLength(3);
  });

  it("offers exactly the roles the matrix lets each role invite", () => {
    for (const [name = "", ...cells] of fleet.rows) {
      if (!name.startsWith("CreateInvite for ")) {
        continue;
      }
      const invited = role(name.slice("CreateInvite for ".length));
      fleet.header.slice(1).forEach((actorLabel, column) => {
        const offered = (inviteRoles(role(actorLabel)) as readonly string[]).includes(invited);

        expect(offered, `${actorLabel} inviting ${invited}`).toBe(cells[column] === "ok");
      });
    }
  });

  it("offers a role this app doesn't know nothing", () => {
    expect(inviteRoles("superuser" as UserRole)).toEqual([]);
  });
});

describe("canRevoke", () => {
  const revoke = table("### RevokeInvite");
  const invite = (inviteRole: UserRole, state: InviteResponse["state"]): InviteResponse => ({
    id: OTHER_ID,
    role: inviteRole,
    state,
    created_by: { id: ACTOR_ID, username: "afkowner" },
    created_at: "2026-10-13T08:00:00.000Z",
    expires_at: "2026-10-16T08:00:00.000Z",
  });

  it("reads the matrix's RevokeInvite table as it is", () => {
    expect(revoke.header).toEqual(["invite for", "Owner", "Admin", "Member"]);
    expect(revoke.rows).toHaveLength(3);
  });

  it("allows exactly what the matrix allows, for every invite and actor", () => {
    for (const [inviteLabel = "", ...cells] of revoke.rows) {
      revoke.header.slice(1).forEach((actorLabel, column) => {
        expect(
          canRevokeRole(role(actorLabel), role(inviteLabel)),
          `${actorLabel} revoking ${inviteLabel}`,
        ).toBe(cells[column] === "ok");
      });
    }
  });

  it("offers it only on a pending invite", () => {
    expect(canRevoke("owner", invite("member", "pending"))).toBe(true);
    for (const state of ["used", "revoked", "expired"] as const) {
      expect(canRevoke("owner", invite("member", state))).toBe(false);
    }
  });
});

describe("serverSettingsAccess", () => {
  const fleet = table("## Fleet-wide");
  const row = (name: string) => fleet.rows.find(([permission]) => permission === name) ?? [];

  it("reads the matrix's two rows of the servers' settings as they are", () => {
    expect(row("ViewServerSettings")).toEqual(["ViewServerSettings", "ok", "ok", "403"]);
    expect(row("ConfigureServerSettings")).toEqual(["ConfigureServerSettings", "ok", "ok", "403"]);
  });

  it("allows exactly what the matrix allows, for every role", () => {
    for (const name of ["ViewServerSettings", "ConfigureServerSettings"] as const) {
      const [, ...cells] = row(name);
      fleet.header.slice(1).forEach((actorLabel, column) => {
        const access = serverSettingsAccess(role(actorLabel));
        const allowed = name === "ViewServerSettings" ? access.view : access.configure;

        expect(allowed, `${actorLabel}: ${name}`).toBe(cells[column] === "ok");
      });
    }
  });

  it("lets in through the admin layout's gate exactly who may read them", () => {
    for (const actorLabel of fleet.header.slice(1)) {
      const actor = role(actorLabel);

      expect(serverSettingsAccess(actor).view, actorLabel).toBe(privilegeOf(actor) === "admin");
    }
  });

  it("gives a role this app doesn't know nothing", () => {
    expect(serverSettingsAccess("superuser" as UserRole)).toEqual({
      view: false,
      configure: false,
    });
  });
});
