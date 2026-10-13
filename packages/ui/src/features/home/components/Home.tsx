// Home (Plan.md P8.6): who is signed in, with which role, on which server. Bots and
// accounts arrive in later phases.
import { useMe, useServerProfile } from "../../../api/queries";
import { displayOrigin } from "../../../lib/origin";
import { roleLabel } from "../../shell/nav";

/** The first signed-in screen. */
export function Home() {
  const { data: user } = useMe();
  const { data: profile } = useServerProfile();
  const origin = profile?.state === "set" ? ` on ${displayOrigin(profile.profile.url)}` : "";
  return (
    <main className="flex flex-col gap-2 p-6">
      <h1 className="text-2xl font-semibold tracking-tight">Home</h1>
      {user === undefined ? null : (
        <p>{`Signed in as ${user.username} (${roleLabel(user.role)})${origin}`}</p>
      )}
      <p className="text-muted-foreground">Bots and accounts arrive in a later version.</p>
    </main>
  );
}
