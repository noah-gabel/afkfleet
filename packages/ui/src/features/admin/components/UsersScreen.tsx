// The users screen (Plan.md P8.8, ADR-0010, ADR-0016, ADR-0017): every user, newest first,
// a page at a time, for Admins and the Owner. Each row offers only what the server would
// allow (permissions.ts): the Owner changes Members' and Admins' roles and disables or
// enables them; an Admin disables or enables Members.
// - A role change and disabling ask first, naming what they end: every session of the user,
//   and with a demotion or a disabling their pending invites. Enabling runs at once.
// - Each change needs a step-up: the row asks for the password when the server wants it
//   (useStepUpAction), and the change goes once more.
// - An account whose 2FA isn't set up was registered but never logged in: whoever first
//   logs in with its password sets it up, so an Admin can disable it (ADR-0016).
// Nothing here carries a secret but the step-up's password, which StepUpForm holds: the list
// is a query and each change a mutation.
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Fragment, useState } from "react";
import { toast } from "sonner";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { USERS_KEY, useMe, useUsers } from "../../../api/queries";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import type { UserPatchRequest } from "../../../generated/UserPatchRequest";
import type { UserResponse } from "../../../generated/UserResponse";
import { formatTime } from "../../../lib/time";
import { roleLabel } from "../../shell/nav";
import { useStepUpAction } from "../hooks/useStepUpAction";
import { type Actor, userActions } from "../permissions";
import { StepUpPrompt } from "./StepUpPrompt";

/** One change a row offers. */
type Change =
  | { readonly kind: "role"; readonly role: "member" | "admin" }
  | { readonly kind: "disabled"; readonly disabled: boolean };

/** What a change's button says. */
function label(change: Change): string {
  if (change.kind === "role") {
    return change.role === "admin" ? "Make admin" : "Make member";
  }
  return change.disabled ? "Disable" : "Enable";
}

/** What asking first says, or nothing for a change that runs at once. */
function question(change: Change, username: string): string | undefined {
  if (change.kind === "role") {
    return change.role === "admin"
      ? `Making ${username} an Admin logs them out everywhere.`
      : `Making ${username} a Member logs them out everywhere and revokes their pending invites.`;
  }
  return change.disabled
    ? `Disabling ${username} logs them out everywhere and revokes their pending invites. They can't log in until they're enabled again.`
    : undefined;
}

/** What the toast says once it's done. */
function doneText(change: Change, username: string): string {
  if (change.kind === "role") {
    return `${username} is now ${change.role === "admin" ? "an Admin" : "a Member"}.`;
  }
  return `${username} is ${change.disabled ? "disabled" : "enabled"}.`;
}

/** The changes `actor` may make to `user`, as buttons. */
function changesFor(actor: Actor, user: UserResponse): readonly Change[] {
  const allowed = userActions(actor, user);
  const changes: Change[] = [];
  if (user.role === "member" && allowed.setRole.admin) {
    changes.push({ kind: "role", role: "admin" });
  }
  if (user.role === "admin" && allowed.setRole.member) {
    changes.push({ kind: "role", role: "member" });
  }
  if (allowed.setDisabled) {
    changes.push({ kind: "disabled", disabled: !user.disabled });
  }
  return changes;
}

/** `/admin/users`. */
export function UsersScreen() {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const { data: actor } = useMe();
  const users = useUsers();
  const refetch = () => void queryClient.invalidateQueries({ queryKey: USERS_KEY });
  const writes = useStepUpAction({ onRefused: refetch });
  const patch = useMutation({
    mutationFn: ({ id, request }: { id: string; request: UserPatchRequest }) =>
      api.patchUser(id, request),
    onSettled: refetch,
  });
  const [asking, setAsking] = useState<{ id: string; change: Change } | null>(null);
  const rows = users.data?.pages.flatMap((page) => page.items) ?? [];
  const busy = writes.running !== null || writes.prompt !== null;

  function run(user: UserResponse, change: Change): void {
    setAsking(null);
    const request: UserPatchRequest =
      change.kind === "role" ? { role: change.role } : { disabled: change.disabled };
    void writes.attempt({
      key: user.id,
      run: async () => {
        await patch.mutateAsync({ id: user.id, request });
        toast.success(doneText(change, user.username));
      },
    });
  }

  function choose(user: UserResponse, change: Change): void {
    if (question(change, user.username) === undefined) {
      run(user, change);
    } else {
      setAsking({ id: user.id, change });
    }
  }

  return (
    <main className="flex flex-col gap-4 p-6">
      <h1 className="text-2xl font-semibold tracking-tight">Users</h1>
      <p className="text-sm text-muted-foreground">
        An account whose 2FA isn't set up was registered but never logged in; whoever first logs in
        with its password sets it up. Disable it if the person you invited doesn't finish.
      </p>
      {users.isError ? (
        <p role="alert" className="text-sm text-destructive">
          {describeError(toApiError(users.error).error)}
        </p>
      ) : null}
      <table aria-label="Users" className="w-full text-left text-sm">
        <thead className="text-muted-foreground">
          <tr>
            <th className="py-1 font-medium">Username</th>
            <th className="py-1 font-medium">Role</th>
            <th className="py-1 font-medium">2FA</th>
            <th className="py-1 font-medium">Status</th>
            <th className="py-1 font-medium">Last seen</th>
            <th className="py-1 font-medium">Created</th>
            <th className="py-1 font-medium">
              <span className="sr-only">Actions</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {rows.map((user) => {
            const isSelf = user.id === actor?.id;
            const changes = actor === undefined ? [] : changesFor(actor, user);
            const pending = asking?.id === user.id ? asking.change : null;
            const failure = writes.failure?.key === user.id ? writes.failure.message : undefined;
            return (
              <Fragment key={user.id}>
                <tr className="border-t">
                  <td className="py-2">{isSelf ? `${user.username} (you)` : user.username}</td>
                  <td className="py-2">{roleLabel(user.role)}</td>
                  <td className="py-2">{user.mfa_enrolled ? "Set up" : "Not set up"}</td>
                  <td className="py-2">{user.disabled ? "Disabled" : "Active"}</td>
                  <td className="py-2">
                    {user.last_seen_at === undefined ? "Never" : formatTime(user.last_seen_at)}
                  </td>
                  <td className="py-2">{formatTime(user.created_at)}</td>
                  <td className="py-2">
                    <div className="flex justify-end gap-2">
                      {changes.map((change) => (
                        <Button
                          key={label(change)}
                          type="button"
                          size="sm"
                          variant="outline"
                          disabled={busy}
                          onClick={() => choose(user, change)}
                        >
                          {label(change)}
                        </Button>
                      ))}
                    </div>
                  </td>
                </tr>
                {pending === null && writes.prompt !== user.id && failure === undefined ? null : (
                  <tr>
                    <td colSpan={7} className="pb-3">
                      {pending === null ? null : (
                        <Alert>
                          <AlertDescription className="flex flex-col gap-2 text-foreground">
                            <p>{question(pending, user.username)}</p>
                            <div className="flex gap-2">
                              <Button
                                type="button"
                                variant="destructive"
                                disabled={busy}
                                onClick={() => run(user, pending)}
                              >
                                {label(pending)}
                              </Button>
                              <Button
                                type="button"
                                variant="outline"
                                onClick={() => setAsking(null)}
                              >
                                Cancel
                              </Button>
                            </div>
                          </AlertDescription>
                        </Alert>
                      )}
                      {writes.prompt === user.id ? (
                        <StepUpPrompt
                          id={`step-up-${user.id}`}
                          pending={writes.running !== null}
                          onConfirm={writes.confirm}
                          onCancel={writes.cancel}
                        />
                      ) : null}
                      {failure === undefined ? null : (
                        <p role="alert" className="text-sm text-destructive">
                          {failure}
                        </p>
                      )}
                    </td>
                  </tr>
                )}
              </Fragment>
            );
          })}
        </tbody>
      </table>
      {users.hasNextPage ? (
        <Button
          type="button"
          variant="outline"
          className="self-start"
          disabled={users.isFetchingNextPage}
          onClick={() => void users.fetchNextPage()}
        >
          Show more
        </Button>
      ) : null}
    </main>
  );
}
