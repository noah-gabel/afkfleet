// The navigation (Plan.md P8.6): one table, each item with the privilege it needs. A role
// comes from the server's open `UserRole`, so a role this app doesn't know gets only a
// Member's privilege, never more.
import type { UserRole } from "../../generated/UserRole";

/** What a role may see in the app. */
export type Privilege = "member" | "admin";

/** One navigation entry. */
export interface NavItem {
  readonly to: "/home" | "/server" | "/profile" | "/admin/users" | "/admin/invites";
  readonly label: string;
  readonly privilege: Privilege;
}

/** Every entry, in display order. */
export const NAV_ITEMS: readonly NavItem[] = [
  { to: "/home", label: "Home", privilege: "member" },
  { to: "/server", label: "Server", privilege: "member" },
  { to: "/profile", label: "Profile", privilege: "member" },
  { to: "/admin/users", label: "Users", privilege: "admin" },
  { to: "/admin/invites", label: "Invites", privilege: "admin" },
];

/** The privilege of `role`; an unknown role is a Member's. */
export function privilegeOf(role: UserRole): Privilege {
  switch (role) {
    case "admin":
    case "owner":
      return "admin";
    default:
      return "member";
  }
}

/** The entries `role` may see. */
export function navItems(role: UserRole): readonly NavItem[] {
  const privilege = privilegeOf(role);
  return NAV_ITEMS.filter((item) => item.privilege === "member" || privilege === "admin");
}

/** How the app names `role`; an unknown one is shown as the server sent it. */
export function roleLabel(role: UserRole): string {
  switch (role) {
    case "member":
      return "Member";
    case "admin":
      return "Admin";
    case "owner":
      return "Owner";
    default:
      return role;
  }
}
