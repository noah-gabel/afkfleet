// The user's sessions (Plan.md P8.7, ADR-0016, ADR-0017): every live login of the account,
// newest first, a page at a time. The server keeps no device, address or user agent, so a
// row is told apart by its times only.
// - This session's "Log out" is the header's logout (`DELETE /me/sessions/{id}` refuses it).
// - Another one's "Log out" ends it at once: whoever used it just logs in again.
// - "Log out all other sessions" asks first, then ends every other session the server lists
//   (useLogOutOthers), the remedy for a lost PC.
// Nothing here carries a secret, so the list is a query and a single logout a mutation.
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { SESSIONS_KEY, useSessions } from "../../../api/queries";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import type { SessionResponse } from "../../../generated/SessionResponse";
import { formatTime } from "../../../lib/time";
import { useLogout } from "../../shell/hooks/useLogout";
import { useLogOutOthers } from "../hooks/useLogOutOthers";

/** The sessions section of the profile. */
export function SessionList() {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const sessions = useSessions();
  const { logOut, pending: loggingOut } = useLogout();
  const others = useLogOutOthers();
  const [confirming, setConfirming] = useState(false);
  const [failure, setFailure] = useState<string>();
  const revoke = useMutation({
    mutationFn: (id: string) => api.revokeSession(id),
    onMutate: () => setFailure(undefined),
    onError: (rejection) => setFailure(describeError(toApiError(rejection).error)),
    onSettled: () => queryClient.invalidateQueries({ queryKey: SESSIONS_KEY }),
  });
  const rows = sessions.data?.pages.flatMap((page) => page.items) ?? [];
  const busy = revoke.isPending || others.running || loggingOut;

  return (
    <section aria-labelledby="sessions-heading" className="flex flex-col gap-3">
      <h2 id="sessions-heading" className="text-lg font-semibold">
        Sessions
      </h2>
      <p className="text-sm text-muted-foreground">
        Every PC where your account is logged in. The server doesn't record which PC a session is,
        only its times.
      </p>
      {sessions.isError ? (
        <p role="alert" className="text-sm text-destructive">
          {describeError(toApiError(sessions.error).error)}
        </p>
      ) : null}
      {failure === undefined ? null : (
        <p role="alert" className="text-sm text-destructive">
          {`Couldn't log that session out: ${failure}`}
        </p>
      )}
      <table aria-label="Sessions" className="w-full text-left text-sm">
        <thead className="text-muted-foreground">
          <tr>
            <th className="py-1 font-medium">Logged in</th>
            <th className="py-1 font-medium">Last used</th>
            <th className="py-1 font-medium">Expires</th>
            <th className="py-1 font-medium">
              <span className="sr-only">Actions</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {rows.map((session) => (
            <Row
              key={session.id}
              session={session}
              busy={busy}
              onLogOut={() => {
                if (session.current) {
                  void logOut();
                } else {
                  revoke.mutate(session.id);
                }
              }}
            />
          ))}
        </tbody>
      </table>
      {sessions.hasNextPage ? (
        <Button
          type="button"
          variant="outline"
          className="self-start"
          disabled={sessions.isFetchingNextPage}
          onClick={() => void sessions.fetchNextPage()}
        >
          Show more
        </Button>
      ) : null}
      {confirming ? (
        <Alert>
          <AlertDescription className="flex flex-col gap-2 text-foreground">
            <p>
              This logs out every other session of your account, on every PC, including ones not
              shown here. Whoever uses them has to log in again.
            </p>
            <div className="flex gap-2">
              <Button
                type="button"
                variant="destructive"
                disabled={busy}
                onClick={() => {
                  setConfirming(false);
                  void others.run();
                }}
              >
                Log out all other sessions
              </Button>
              <Button type="button" variant="outline" onClick={() => setConfirming(false)}>
                Cancel
              </Button>
            </div>
          </AlertDescription>
        </Alert>
      ) : (
        <Button
          type="button"
          variant="outline"
          className="self-start"
          disabled={busy}
          onClick={() => setConfirming(true)}
        >
          Log out all other sessions
        </Button>
      )}
      {others.running ? (
        <p role="status" className="text-sm text-muted-foreground">
          Logging out your other sessions…
        </p>
      ) : null}
      {others.report === undefined ? null : (
        <p role="status" className="text-sm">
          {others.report.text}
        </p>
      )}
    </section>
  );
}

function Row({
  session,
  busy,
  onLogOut,
}: {
  session: SessionResponse;
  busy: boolean;
  onLogOut: () => void;
}) {
  const created = formatTime(session.created_at);
  return (
    <tr className="border-t">
      <td className="py-2">{created}</td>
      <td className="py-2">{formatTime(session.last_used_at)}</td>
      <td className="py-2">{formatTime(session.expires_at)}</td>
      <td className="py-2">
        <div className="flex items-center justify-end gap-2">
          {session.current ? <span className="text-muted-foreground">This session</span> : null}
          <Button
            type="button"
            size="sm"
            variant="outline"
            disabled={busy}
            aria-label={
              session.current ? "Log out this session" : `Log out the session logged in ${created}`
            }
            onClick={onLogOut}
          >
            Log out
          </Button>
        </div>
      </td>
    </tr>
  );
}
