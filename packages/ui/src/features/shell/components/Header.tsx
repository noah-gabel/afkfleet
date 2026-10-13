// The header of the signed-in screens (Plan.md P8.6): the app's name, who is signed in
// with which role, and "Log out" (useLogout).
import { LogOutIcon } from "lucide-react";

import { useMe } from "../../../api/queries";
import { Button } from "../../../components/ui/button";
import { useLogout } from "../hooks/useLogout";
import { roleLabel } from "../nav";

/** The header. */
export function Header() {
  const { data: user } = useMe();
  const { logOut, pending } = useLogout();
  return (
    <header className="flex items-center justify-between border-b px-4 py-2">
      <span className="font-semibold tracking-tight">afkfleet</span>
      <div className="flex items-center gap-3 text-sm">
        {user === undefined ? null : (
          <>
            <span className="font-medium">{user.username}</span>
            <span className="text-muted-foreground">{roleLabel(user.role)}</span>
          </>
        )}
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={pending}
          onClick={() => {
            void logOut();
          }}
        >
          <LogOutIcon aria-hidden />
          Log out
        </Button>
      </div>
    </header>
  );
}
