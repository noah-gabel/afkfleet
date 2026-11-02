// The frame of every signed-in screen (Plan.md P8.6, P11.11; ADR-0020, U91): the sidebar on the left,
// the screen beside it, in the room the fixed sidebar leaves. The stack of banner and live hint is
// the root's, so it keeps running across a login and a logout (rootRoute.tsx).
import { Outlet } from "@tanstack/react-router";

import { LiveNotices } from "../../bots/components/LiveNotices";
import { Sidebar } from "./Sidebar";

/** The signed-in layout. */
export function AppLayout() {
  return (
    <>
      <LiveNotices />
      <Sidebar />
      <div className="flex min-w-0 flex-1 flex-col pl-52 max-rail:pl-16">
        <Outlet />
      </div>
    </>
  );
}
