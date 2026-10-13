// The frame of every signed-in screen (Plan.md P8.6): the header above, the navigation on
// the left, the screen beside it.
import { Outlet } from "@tanstack/react-router";

import { Header } from "./Header";
import { Nav } from "./Nav";

/** The signed-in layout. */
export function AppLayout() {
  return (
    <div className="flex flex-1 flex-col">
      <Header />
      <div className="flex flex-1">
        <Nav />
        <div className="flex flex-1 flex-col">
          <Outlet />
        </div>
      </div>
    </div>
  );
}
