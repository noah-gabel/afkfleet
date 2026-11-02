// The frame of every signed-in screen (Plan.md P8.6, P11.11; ADR-0020, U91, S123): the sidebar on the
// left, the screen beside it, in the room the fixed sidebar leaves. The stack of banner and live hint
// is the root's, so it keeps running across a login and a logout (rootRoute.tsx).
//
// The column is the one thing that scrolls, on both axes, so wide content never slides under the rail
// and the page never scrolls. It's the container the screens' layouts query. Chromium makes a scroller
// with no focusable child a Tab stop, whose outline would otherwise lie outside the window.
//
// Over the column, one row holds the way back (U111, S126). It keeps its height with nothing to go
// back to, so a screen's heading is at the same place on every screen, and it never scrolls away.
import { Outlet } from "@tanstack/react-router";
import { ArrowLeftIcon } from "lucide-react";

import { Button } from "../../../components/ui/button";
import { LiveNotices } from "../../bots/components/LiveNotices";
import { useBack } from "../hooks/useBack";
import { Sidebar } from "./Sidebar";

/** The way back's text. */
export const BACK = "Back";

/** The signed-in layout. */
export function AppLayout() {
  const { canGoBack, goBack } = useBack();
  return (
    <>
      <LiveNotices />
      <Sidebar />
      <div className="ml-52 flex min-h-0 min-w-0 flex-1 flex-col max-rail:ml-16">
        <div data-slot="back-row" className="flex h-10 shrink-0 items-center px-4">
          {canGoBack ? (
            <Button type="button" variant="ghost" size="sm" onClick={goBack}>
              <ArrowLeftIcon aria-hidden="true" />
              {BACK}
            </Button>
          ) : null}
        </div>
        <div
          data-slot="content"
          style={{ outlineOffset: -2 }}
          className="@container/content flex min-h-0 flex-1 flex-col overflow-auto scroll-p-1 [&>main]:pt-1"
        >
          <Outlet />
        </div>
      </div>
    </>
  );
}
