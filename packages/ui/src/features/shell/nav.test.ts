import { describe, expect, it } from "vitest";

import type { UserRole } from "../../generated/UserRole";
import { navItems, privilegeOf, roleLabel } from "./nav";

describe("privilegeOf", () => {
  it.each<[UserRole, string]>([
    ["member", "member"],
    ["admin", "admin"],
    ["owner", "admin"],
    ["superuser" as UserRole, "member"],
  ])("gives %s the %s privilege", (role, privilege) => {
    expect(privilegeOf(role)).toBe(privilege);
  });
});

describe("navItems", () => {
  it("lists Home, Accounts, Server and Profile for a Member", () => {
    expect(navItems("member").map((item) => item.label)).toEqual([
      "Home",
      "Accounts",
      "Server",
      "Profile",
    ]);
  });

  it("adds the admin screens for an Admin", () => {
    expect(navItems("admin").map((item) => item.to)).toEqual([
      "/home",
      "/accounts",
      "/server",
      "/profile",
      "/admin/users",
      "/admin/invites",
      "/admin/servers",
    ]);
  });
});

describe("roleLabel", () => {
  it.each<[UserRole, string]>([
    ["member", "Member"],
    ["admin", "Admin"],
    ["owner", "Owner"],
    ["superuser" as UserRole, "superuser"],
  ])("names %s %s", (role, label) => {
    expect(roleLabel(role)).toBe(label);
  });
});
