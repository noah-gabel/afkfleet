// The invites screen (Plan.md P8.8, ADR-0016, ADR-0017): every invite in its state, newest
// first, a page at a time, and a new one.
// - The form offers only the roles the actor may invite (permissions.ts): the Owner a
//   Member or an Admin, an Admin a Member. Creating needs a step-up, asked for when the
//   server wants it (useStepUpAction).
// - The code is shown once. `createInvite` is a direct call, never a mutation, since its
//   answer carries the code (CLAUDE.md); the code lives only in this screen's state, with
//   "Copy invitation" (the server, the code, its end and the CA's fingerprints) and "Copy
//   code", until "Done" or the user leaves.
// - Revoking a pending invite the actor may revoke asks first, then needs the step-up too.
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Fragment, useState } from "react";
import { toast } from "sonner";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { INVITES_KEY, useInvites, useMe, useServerProfile } from "../../../api/queries";
import { Alert, AlertDescription, AlertTitle } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import type { CommandError } from "../../../generated/CommandError";
import type { CreatedInviteResponse } from "../../../generated/CreatedInviteResponse";
import type { InviteResponse } from "../../../generated/InviteResponse";
import type { InviteState } from "../../../generated/InviteState";
import { copyWithToast } from "../../../lib/clipboard";
import { displayOrigin } from "../../../lib/origin";
import { formatTime, formatTimeWithZone } from "../../../lib/time";
import { roleLabel } from "../../shell/nav";
import { useStepUpAction } from "../hooks/useStepUpAction";
import { invitationText } from "../invitation";
import { canRevoke, inviteRoles } from "../permissions";
import { StepUpPrompt } from "./StepUpPrompt";

/** At most 20 pending invites per creator: the server's 409. */
export const INVITE_CAP =
  "You have 20 pending invites. Revoke one, or wait until one is used or expires.";
/** The toast after "Copy invitation". */
export const INVITATION_COPIED =
  "Invitation copied. Send it, then copy something else: Windows' clipboard history may keep a copy.";
/** The toast after "Copy code". */
export const CODE_COPIED =
  "Code copied. Send it, then copy something else: Windows' clipboard history may keep a copy.";
/** The toast when copying failed. */
export const COPY_FAILED = "Couldn't copy. Select the code and copy it by hand.";

/** The key of the create form's write. */
const CREATE = "create";

/** How an invite's state reads; one this app doesn't know shows as the server sent it. */
function stateLabel(state: InviteState): string {
  switch (state) {
    case "pending":
      return "Pending";
    case "used":
      return "Used";
    case "revoked":
      return "Revoked";
    case "expired":
      return "Expired";
    default:
      return state;
  }
}

/** A refusal's text: the cap in plain words, anything else describeError's. */
function describeRefusal(error: CommandError): string {
  return error.api?.status === 409 && error.api.code === "conflict"
    ? INVITE_CAP
    : describeError(error);
}

/** `/admin/invites`. */
export function InvitesScreen() {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const { data: actor } = useMe();
  const invites = useInvites();
  const refetch = () => void queryClient.invalidateQueries({ queryKey: INVITES_KEY });
  const writes = useStepUpAction({ describe: describeRefusal, onRefused: refetch });
  const revoke = useMutation({
    mutationFn: (id: string) => api.revokeInvite(id),
    onSettled: refetch,
  });
  const [created, setCreated] = useState<CreatedInviteResponse | null>(null);
  const [asking, setAsking] = useState<string | null>(null);
  const roles = actor === undefined ? [] : inviteRoles(actor.role);
  const [role, setRole] = useState<"member" | "admin">("member");
  const rows = invites.data?.pages.flatMap((page) => page.items) ?? [];
  const busy = writes.running !== null || writes.prompt !== null;

  function create(): void {
    setCreated(null);
    const chosen = roles.includes(role) ? role : "member";
    void writes.attempt({
      key: CREATE,
      run: async () => {
        const answer = await api.createInvite({ role: chosen });
        setCreated(answer);
        refetch();
      },
    });
  }

  function revokeNow(invite: InviteResponse): void {
    setAsking(null);
    void writes.attempt({
      key: invite.id,
      run: async () => {
        await revoke.mutateAsync(invite.id);
        toast.success("The invite was revoked.");
      },
    });
  }

  return (
    <main className="flex flex-col gap-4 p-6">
      <h1 className="text-2xl font-semibold tracking-tight">Invites</h1>
      {roles.length === 0 ? null : (
        <section aria-labelledby="new-invite-heading" className="flex max-w-xl flex-col gap-3">
          <h2 id="new-invite-heading" className="text-lg font-semibold">
            New invite
          </h2>
          {roles.length === 1 ? (
            <p className="text-sm">The invite is for a Member.</p>
          ) : (
            <fieldset className="flex gap-4 text-sm">
              <legend className="sr-only">The invite is for</legend>
              {roles.map((option) => (
                <label key={option} className="flex items-center gap-2">
                  <input
                    type="radio"
                    name="invite-role"
                    className="size-4 accent-primary"
                    checked={role === option}
                    onChange={() => setRole(option)}
                  />
                  {roleLabel(option)}
                </label>
              ))}
            </fieldset>
          )}
          <Button type="button" className="self-start" disabled={busy} onClick={create}>
            Create invite
          </Button>
          {writes.prompt === CREATE ? (
            <StepUpPrompt
              id="step-up-create"
              pending={writes.running !== null}
              onConfirm={writes.confirm}
              onCancel={writes.cancel}
            />
          ) : null}
          {writes.failure?.key === CREATE ? (
            <p role="alert" className="text-sm text-destructive">
              {writes.failure.message}
            </p>
          ) : null}
          {created === null ? null : (
            <CreatedInvite created={created} onDone={() => setCreated(null)} />
          )}
        </section>
      )}
      {invites.isError ? (
        <p role="alert" className="text-sm text-destructive">
          {describeError(toApiError(invites.error).error)}
        </p>
      ) : null}
      <table aria-label="Invites" className="w-full text-left text-sm">
        <thead className="text-muted-foreground">
          <tr>
            <th className="py-1 font-medium">Role</th>
            <th className="py-1 font-medium">State</th>
            <th className="py-1 font-medium">Created by</th>
            <th className="py-1 font-medium">Created</th>
            <th className="py-1 font-medium">Expires</th>
            <th className="py-1 font-medium">Used by</th>
            <th className="py-1 font-medium">
              <span className="sr-only">Actions</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {rows.map((invite) => {
            const revocable = actor !== undefined && canRevoke(actor.role, invite);
            const failure = writes.failure?.key === invite.id ? writes.failure.message : undefined;
            const extra =
              asking === invite.id || writes.prompt === invite.id || failure !== undefined;
            return (
              <Fragment key={invite.id}>
                <tr className="border-t">
                  <td className="py-2">{roleLabel(invite.role)}</td>
                  <td className="py-2">{stateLabel(invite.state)}</td>
                  <td className="py-2">{invite.created_by.username}</td>
                  <td className="py-2">{formatTime(invite.created_at)}</td>
                  <td className="py-2">{formatTime(invite.expires_at)}</td>
                  <td className="py-2">{invite.used_by?.username ?? ""}</td>
                  <td className="py-2">
                    <div className="flex justify-end">
                      {revocable ? (
                        <Button
                          type="button"
                          size="sm"
                          variant="outline"
                          disabled={busy}
                          onClick={() => setAsking(invite.id)}
                        >
                          Revoke
                        </Button>
                      ) : null}
                    </div>
                  </td>
                </tr>
                {extra ? (
                  <tr>
                    <td colSpan={7} className="pb-3">
                      {asking === invite.id ? (
                        <Alert>
                          <AlertDescription className="flex flex-col gap-2 text-foreground">
                            <p>Revoking this invite means its code can't be used to register.</p>
                            <div className="flex gap-2">
                              <Button
                                type="button"
                                variant="destructive"
                                disabled={busy}
                                onClick={() => revokeNow(invite)}
                              >
                                Revoke
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
                      ) : null}
                      {writes.prompt === invite.id ? (
                        <StepUpPrompt
                          id={`step-up-${invite.id}`}
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
                ) : null}
              </Fragment>
            );
          })}
        </tbody>
      </table>
      {invites.hasNextPage ? (
        <Button
          type="button"
          variant="outline"
          className="self-start"
          disabled={invites.isFetchingNextPage}
          onClick={() => void invites.fetchNextPage()}
        >
          Show more
        </Button>
      ) : null}
    </main>
  );
}

/** A new invite's code, shown this once. */
function CreatedInvite({
  created,
  onDone,
}: {
  created: CreatedInviteResponse;
  onDone: () => void;
}) {
  const { data: profile } = useServerProfile();
  const server = profile?.state === "set" ? profile.profile : undefined;
  const invitation = invitationText({
    origin: server === undefined ? "" : displayOrigin(server.url),
    code: created.code,
    expiresAt: created.invite.expires_at,
    ...(server?.custom_ca === undefined ? {} : { fingerprints: server.custom_ca.fingerprints }),
  });
  return (
    <Alert>
      <AlertTitle>{`New invite for a ${roleLabel(created.invite.role)}`}</AlertTitle>
      <AlertDescription className="flex flex-col gap-2 text-foreground">
        <p>
          Send this code to the person you invite. It's shown only this once; the server keeps only
          its hash.
        </p>
        <code className="rounded-md bg-muted p-2 font-mono text-sm select-all">{created.code}</code>
        <p>{`It works once, until ${formatTimeWithZone(created.invite.expires_at)}.`}</p>
        <div className="flex gap-2">
          <Button
            type="button"
            variant="outline"
            onClick={() => void copyWithToast(invitation, INVITATION_COPIED, COPY_FAILED)}
          >
            Copy invitation
          </Button>
          <Button
            type="button"
            variant="outline"
            onClick={() => void copyWithToast(created.code, CODE_COPIED, COPY_FAILED)}
          >
            Copy code
          </Button>
          <Button type="button" onClick={onDone}>
            Done
          </Button>
        </div>
      </AlertDescription>
    </Alert>
  );
}
