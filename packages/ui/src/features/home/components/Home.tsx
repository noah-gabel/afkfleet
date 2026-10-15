// Home (Plan.md P8.6, P8.7): who is signed in, with which role, on which server, and the
// account's warnings, since every sign-in lands here, and for Admins and the Owner how many
// Microsoft links wait for their approval (P9.8). Bots arrive in a later phase.
import { useMe, useServerProfile } from "../../../api/queries";
import { displayOrigin } from "../../../lib/origin";
import { WaitingAlert } from "../../accounts/components/WaitingAlert";
import { SecurityWarnings } from "../../profile/components/SecurityWarnings";
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
        <>
          <p>{`Signed in as ${user.username} (${roleLabel(user.role)})${origin}`}</p>
          <SecurityWarnings user={user} place="home" />
          <WaitingAlert />
        </>
      )}
      <p className="text-muted-foreground">Bots arrive in a later version.</p>
    </main>
  );
}
