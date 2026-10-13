// The profile (Plan.md P8.7, ADR-0016): the user's own account, its warnings, the two flows
// (changing the password, replacing the authenticator) and the session list.
import { Link } from "@tanstack/react-router";

import { useMe } from "../../../api/queries";
import { Button } from "../../../components/ui/button";
import { formatTime } from "../../../lib/time";
import { roleLabel } from "../../shell/nav";
import { SecurityWarnings } from "./SecurityWarnings";
import { SessionList } from "./SessionList";

/** `/profile`. */
export function ProfileOverview() {
  const { data: user } = useMe();
  return (
    <main className="flex max-w-3xl flex-col gap-6 p-6">
      <h1 className="text-2xl font-semibold tracking-tight">Profile</h1>
      {user === undefined ? (
        <p className="text-muted-foreground">Loading…</p>
      ) : (
        <>
          <SecurityWarnings user={user} place="profile" />
          <section aria-labelledby="account-heading" className="flex flex-col gap-2">
            <h2 id="account-heading" className="text-lg font-semibold">
              Account
            </h2>
            <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-1 text-sm">
              <dt className="text-muted-foreground">Username</dt>
              <dd>{user.username}</dd>
              <dt className="text-muted-foreground">Role</dt>
              <dd>{roleLabel(user.role)}</dd>
              <dt className="text-muted-foreground">Password changed</dt>
              <dd>{formatTime(user.password_changed_at)}</dd>
              <dt className="text-muted-foreground">Two-factor authentication</dt>
              <dd>
                {user.mfa_confirmed_at === undefined
                  ? "Not set up"
                  : `Set up ${formatTime(user.mfa_confirmed_at)}`}
              </dd>
            </dl>
          </section>
          <section aria-labelledby="password-heading" className="flex flex-col gap-2">
            <h2 id="password-heading" className="text-lg font-semibold">
              Password
            </h2>
            <p className="text-sm text-muted-foreground">
              Changing it logs out every other session of your account.
            </p>
            <Button asChild variant="outline" className="self-start">
              <Link to="/profile/password">Change password</Link>
            </Button>
          </section>
          <section aria-labelledby="mfa-heading" className="flex flex-col gap-2">
            <h2 id="mfa-heading" className="text-lg font-semibold">
              Two-factor authentication
            </h2>
            <p className="text-sm">
              {`${user.recovery_codes_left} recovery ${user.recovery_codes_left === 1 ? "code" : "codes"} left.`}
            </p>
            <p className="text-sm text-muted-foreground">
              Replacing the authenticator gives you 10 new recovery codes and logs out every other
              session.
            </p>
            <Button asChild variant="outline" className="self-start">
              <Link to="/profile/2fa">Replace authenticator</Link>
            </Button>
          </section>
          <SessionList />
        </>
      )}
    </main>
  );
}
