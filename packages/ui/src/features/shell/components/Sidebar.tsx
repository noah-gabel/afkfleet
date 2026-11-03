// The frame's sidebar (Plan.md P11.11; ADR-0020, U91, U98, S104–S106): the wordmark, the entries the
// user's role may see (nav.ts) with the admin screens under their own heading, and at the bottom a
// box with the user's name and role that opens Profile, beside Log out. Until `me` has answered,
// only a Member's entries show. Accounts carries the count of links waiting for an Admin's or the
// Owner's approval.
//
// Under the `rail` breakpoint it collapses to icons, by CSS alone: there's one DOM, so what's only
// seen goes and every name stays, as `max-rail:sr-only` keeps a label in the accessibility tree. It's
// `fixed`, so the page scrolls beside it; AppLayout and the sticky stack in rootRoute.tsx leave its
// room. The nav scrolls only if the text is zoomed: the minimum window holds it all. Its padding
// keeps a focused entry's outline from being clipped by that scroller.
import { Link } from "@tanstack/react-router";
import {
  ActivityIcon,
  ContainerIcon,
  HouseIcon,
  LogOutIcon,
  type LucideIcon,
  ScrollTextIcon,
  ServerIcon,
  TicketIcon,
  UserCogIcon,
  UserRoundIcon,
  UsersIcon,
  WorkflowIcon,
} from "lucide-react";

import { useMe } from "../../../api/queries";
import { Logo, Wordmark } from "../../../components/Brand";
import { Tooltip } from "../../../components/Tooltip";
import { Button } from "../../../components/ui/button";
import {
  accountsLinkName,
  useWaitingCount,
  WaitingBadge,
} from "../../accounts/components/WaitingAlert";
import { useLogout } from "../hooks/useLogout";
import { type NavItem, navItems, roleLabel } from "../nav";

const ICONS: Record<NavItem["to"], LucideIcon> = {
  "/home": HouseIcon,
  "/modes": WorkflowIcon,
  "/accounts": UsersIcon,
  "/server": ServerIcon,
  "/profile": UserRoundIcon,
  "/admin/users": UserCogIcon,
  "/admin/invites": TicketIcon,
  "/admin/servers": ContainerIcon,
  "/admin/agents": ActivityIcon,
  "/admin/audit": ScrollTextIcon,
};

/** The sidebar. */
export function Sidebar() {
  const { data: user } = useMe();
  const items = navItems(user?.role ?? "member");
  const everyone = items.filter((item) => item.privilege === "member");
  const admin = items.filter((item) => item.privilege === "admin");
  return (
    <aside className="fixed inset-y-0 left-0 z-30 flex w-52 flex-col gap-3 border-r bg-sidebar p-3 text-sm text-sidebar-foreground max-rail:w-16 max-rail:p-2">
      <div className="flex h-8 shrink-0 items-center gap-2 px-3 max-rail:justify-center max-rail:px-0">
        <Logo />
        <span className="text-base tracking-tight max-rail:sr-only">
          <Wordmark />
        </span>
      </div>
      <nav
        aria-label="Main"
        className="relative -m-1 flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-1"
      >
        <Entries items={everyone} />
        {admin.length === 0 ? null : (
          <div className="flex flex-col gap-1">
            <p className="px-3 text-xs font-semibold text-muted-foreground max-rail:sr-only">
              Admin
            </p>
            <Entries items={admin} />
          </div>
        )}
      </nav>
      <Account />
    </aside>
  );
}

function Entries({ items }: { items: readonly NavItem[] }) {
  const waiting = useWaitingCount();
  return (
    <ul className="flex flex-col gap-1">
      {items.map((item) => {
        const Icon = ICONS[item.to];
        const name =
          (item.to === "/accounts" ? accountsLinkName(waiting) : undefined) ?? item.label;
        return (
          <li key={item.to}>
            <Tooltip label={name} className="w-full" tipClassName="rail:hidden">
              <Link
                to={item.to}
                aria-label={item.to === "/accounts" ? accountsLinkName(waiting) : undefined}
                className="relative flex w-full items-center gap-3 rounded-lg px-3 py-2 hover:bg-accent data-[status=active]:bg-accent data-[status=active]:font-semibold data-[status=active]:text-primary max-rail:justify-center max-rail:px-0"
              >
                <Icon aria-hidden className="size-4.5 shrink-0" />
                <span className="max-rail:sr-only">{item.label}</span>
                {item.to === "/accounts" ? <WaitingBadge count={waiting} /> : null}
              </Link>
            </Tooltip>
          </li>
        );
      })}
    </ul>
  );
}

/** Who's signed in, which opens Profile, and Log out. */
function Account() {
  const { data: user } = useMe();
  const { logOut, pending } = useLogout();
  return (
    <fieldset
      aria-label="Account"
      className="flex min-w-0 shrink-0 items-center gap-1 rounded-lg border bg-card p-1 max-rail:flex-col max-rail:border-0 max-rail:bg-transparent max-rail:p-0"
    >
      {user === undefined ? null : (
        <Tooltip
          label={`${user.username}, ${roleLabel(user.role)}`}
          className="min-w-0 flex-1 max-rail:w-full max-rail:flex-none"
          tipClassName="rail:hidden"
        >
          <Link
            to="/profile"
            aria-label={`${user.username}, ${roleLabel(user.role)}, Profile`}
            className="block w-full min-w-0 rounded-md px-2 py-1 hover:bg-accent max-rail:px-0 max-rail:py-1.5 max-rail:text-center"
          >
            <span className="block truncate font-semibold max-rail:sr-only">{user.username}</span>
            <span className="block truncate text-xs text-muted-foreground max-rail:sr-only">
              {roleLabel(user.role)}
            </span>
            <span
              aria-hidden
              className="hidden font-mono text-xs font-semibold uppercase max-rail:block"
            >
              {user.username.slice(0, 2)}
            </span>
          </Link>
        </Tooltip>
      )}
      <Tooltip label="Log out" className="shrink-0">
        <Button
          type="button"
          variant="ghost"
          size="icon"
          aria-label="Log out"
          disabled={pending}
          onClick={() => {
            void logOut();
          }}
        >
          <LogOutIcon aria-hidden />
        </Button>
      </Tooltip>
    </fieldset>
  );
}
