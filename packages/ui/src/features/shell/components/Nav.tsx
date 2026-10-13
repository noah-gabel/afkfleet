// The navigation (Plan.md P8.6): the entries the user's role may see (nav.ts), the admin
// screens under their own heading. Until `me` has answered, only a Member's entries show.
import { Link } from "@tanstack/react-router";

import { useMe } from "../../../api/queries";
import { type NavItem, navItems } from "../nav";

/** The navigation. */
export function Nav() {
  const { data: user } = useMe();
  const items = navItems(user?.role ?? "member");
  const everyone = items.filter((item) => item.privilege === "member");
  const admin = items.filter((item) => item.privilege === "admin");
  return (
    <nav aria-label="Main" className="flex w-48 shrink-0 flex-col gap-4 border-r p-2 text-sm">
      <Entries items={everyone} />
      {admin.length === 0 ? null : (
        <div className="flex flex-col gap-1">
          <p className="px-2 text-xs font-medium text-muted-foreground uppercase">Admin</p>
          <Entries items={admin} />
        </div>
      )}
    </nav>
  );
}

function Entries({ items }: { items: readonly NavItem[] }) {
  return (
    <ul className="flex flex-col gap-1">
      {items.map((item) => (
        <li key={item.to}>
          <Link
            to={item.to}
            className="block rounded-md px-2 py-1.5 hover:bg-muted data-[status=active]:bg-muted data-[status=active]:font-medium"
          >
            {item.label}
          </Link>
        </li>
      ))}
    </ul>
  );
}
