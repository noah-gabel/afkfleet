// The invites screen (Plan.md P8.8, P11.11; ADR-0016, ADR-0017, ADR-0020, U122, U123): every invite
// in its state, newest first, a page at a time, and a new one.
// - "New invite", at the top right as Accounts' add buttons are, opens a box above the table with
//   the roles the actor may invite (permissions.ts): the Owner a Member or an Admin, an Admin a
//   Member. Creating asks for no password: a session is enough (useWriteAction, ADR-0028).
// - The code is shown once, in that box. `createInvite` is a direct call, never a mutation, since
//   its answer carries the code (CLAUDE.md); the code lives only in this screen's state, with
//   "Copy invitation" (the server, the code, its end and the CA's fingerprints) and "Copy code",
//   until "Done" closes the box or the user leaves.
// - Revoking a pending invite the actor may revoke asks first.
// - In a small window the table drops Created, then Created by, then Used by (U122, R228).
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { PlusIcon } from "lucide-react";
import { Fragment, useState } from "react";
import { toast } from "sonner";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { INVITES_KEY, useInvites, useMe, useServerProfile } from "../../../api/queries";
import { ScreenState } from "../../../components/ScreenState";
import { StatusLine, type Tone } from "../../../components/StatusLine";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  TableSpanRow,
} from "../../../components/ui/table";
import type { CommandError } from "../../../generated/CommandError";
import type { CreatedInviteResponse } from "../../../generated/CreatedInviteResponse";
import type { InviteResponse } from "../../../generated/InviteResponse";
import type { InviteState } from "../../../generated/InviteState";
import { copyWithToast } from "../../../lib/clipboard";
import { displayOrigin } from "../../../lib/origin";
import { formatTime, formatTimeWithZone } from "../../../lib/time";
import { roleLabel } from "../../shell/nav";
import { useWriteAction } from "../hooks/useWriteAction";
import { invitationText } from "../invitation";
import { canRevoke, inviteRoles } from "../permissions";

/** At most 20 pending invites per creator: the server's 409. */
export const INVITE_CAP = "20 invites pending. Revoke one, or wait for one to be used or expire.";
/** The toast after "Copy invitation" or "Copy code". */
export const INVITATION_COPIED =
  "Copied. Send it, then copy something else: Windows' clipboard history may keep a copy.";
export const CODE_COPIED = INVITATION_COPIED;
/** The toast when copying failed. */
export const COPY_FAILED = "Copy failed. Select the code by hand.";
const NEW_INVITE = "New invite";
const LOADING_INVITES = "Loading invites…";
const NO_INVITES = "No invites.";

/** Why the invites couldn't be read. */
function invitesFailed(why: string): string {
  return `Couldn't load invites: ${why}`;
}

/** The key of the create form's write. */
const CREATE = "create";

/** The table's columns, which a row's question spans. */
const COLUMNS = 7;

/** How an invite's state reads, and its tone; one this app doesn't know shows as sent (R229). */
function stateLine(state: InviteState): { text: string; tone: Tone } {
  switch (state) {
    case "pending":
      return { text: "Pending", tone: "info" };
    case "used":
      return { text: "Used", tone: "success" };
    case "revoked":
      return { text: "Revoked", tone: "neutral" };
    case "expired":
      return { text: "Expired", tone: "neutral" };
    default:
      return { text: state, tone: "neutral" };
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
  const writes = useWriteAction({ describe: describeRefusal, onRefused: refetch });
  const revoke = useMutation({
    mutationFn: (id: string) => api.revokeInvite(id),
    onSettled: refetch,
  });
  const [open, setOpen] = useState(false);
  const [created, setCreated] = useState<CreatedInviteResponse | null>(null);
  const [asking, setAsking] = useState<string | null>(null);
  const roles = actor === undefined ? [] : inviteRoles(actor.role);
  const [role, setRole] = useState<"member" | "admin">("member");
  const rows = invites.data?.pages.flatMap((page) => page.items) ?? [];
  const busy = writes.running !== null;

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

  function close(): void {
    writes.forget(CREATE);
    setCreated(null);
    setOpen(false);
  }

  function revokeNow(invite: InviteResponse): void {
    setAsking(null);
    void writes.attempt({
      key: invite.id,
      run: async () => {
        await revoke.mutateAsync(invite.id);
        toast.success("Invite revoked.");
      },
    });
  }

  return (
    <main className="flex flex-1 flex-col gap-4 p-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h1 className="text-lg font-semibold tracking-tight">Invites</h1>
        {roles.length === 0 ? null : (
          <Button
            type="button"
            size="sm"
            aria-expanded={open}
            aria-controls="new-invite"
            disabled={open}
            onClick={() => setOpen(true)}
          >
            <PlusIcon aria-hidden="true" />
            {NEW_INVITE}
          </Button>
        )}
      </div>
      {open && roles.length > 0 ? (
        <section
          id="new-invite"
          aria-labelledby="new-invite-heading"
          className="flex max-w-xl flex-col gap-3 rounded-lg border bg-card p-3"
        >
          <h2 id="new-invite-heading" className="text-base font-semibold">
            {NEW_INVITE}
          </h2>
          {created === null ? (
            <>
              {roles.length === 1 ? (
                <p className="text-sm">For a Member.</p>
              ) : (
                <fieldset className="flex gap-4 text-sm">
                  <legend className="sr-only">For</legend>
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
              <div className="flex gap-2">
                <Button type="button" disabled={busy} onClick={create}>
                  Create invite
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  disabled={writes.running === CREATE}
                  onClick={close}
                >
                  Cancel
                </Button>
              </div>
            </>
          ) : (
            <CreatedInvite created={created} onDone={close} />
          )}
          {writes.failure?.key === CREATE ? (
            <p role="alert" className="text-sm text-destructive">
              {writes.failure.message}
            </p>
          ) : null}
        </section>
      ) : null}
      {invites.isError ? (
        <ScreenState kind="error">
          {invitesFailed(describeError(toApiError(invites.error).error))}
        </ScreenState>
      ) : null}
      {invites.isPending ? <ScreenState kind="loading">{LOADING_INVITES}</ScreenState> : null}
      {invites.isSuccess && rows.length === 0 ? (
        <ScreenState kind="empty">{NO_INVITES}</ScreenState>
      ) : null}
      {rows.length === 0 ? null : (
        <Table aria-label="Invites">
          <TableHeader>
            <TableRow>
              <TableHead>Role</TableHead>
              <TableHead>State</TableHead>
              <TableHead drop="under-960">Created by</TableHead>
              <TableHead drop="under-1100">Created</TableHead>
              <TableHead>Expires</TableHead>
              <TableHead drop="under-790">Used by</TableHead>
              <TableHead>
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((invite) => {
              const revocable = actor !== undefined && canRevoke(actor.role, invite);
              const failure =
                writes.failure?.key === invite.id ? writes.failure.message : undefined;
              const extra = asking === invite.id || failure !== undefined;
              const state = stateLine(invite.state);
              return (
                <Fragment key={invite.id}>
                  <TableRow>
                    <TableCell>{roleLabel(invite.role)}</TableCell>
                    <TableCell>
                      <StatusLine tone={state.tone}>{state.text}</StatusLine>
                    </TableCell>
                    <TableCell drop="under-960" className="font-mono break-all">
                      {invite.created_by.username}
                    </TableCell>
                    <TableCell drop="under-1100">{formatTime(invite.created_at)}</TableCell>
                    <TableCell>{formatTime(invite.expires_at)}</TableCell>
                    <TableCell drop="under-790" className="font-mono break-all">
                      {invite.used_by?.username ?? ""}
                    </TableCell>
                    <TableCell>
                      <div className="flex justify-end">
                        {revocable ? (
                          <Button
                            type="button"
                            size="sm"
                            variant="ghost"
                            disabled={busy}
                            onClick={() => setAsking(invite.id)}
                          >
                            Revoke
                          </Button>
                        ) : null}
                      </div>
                    </TableCell>
                  </TableRow>
                  {extra ? (
                    <TableSpanRow span={COLUMNS} className="pb-3">
                      <div className="flex flex-col gap-2">
                        {asking === invite.id ? (
                          <Alert>
                            <AlertDescription className="flex flex-col gap-2 text-foreground">
                              <p>Revoke this invite? Its code then can't register.</p>
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
                        {failure === undefined ? null : (
                          <p role="alert" className="text-sm text-destructive">
                            {failure}
                          </p>
                        )}
                      </div>
                    </TableSpanRow>
                  ) : null}
                </Fragment>
              );
            })}
          </TableBody>
        </Table>
      )}
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
    <div className="flex flex-col gap-2 text-sm">
      <p className="font-semibold">
        {`New invite for ${created.invite.role === "admin" ? "an" : "a"} ${roleLabel(created.invite.role)}`}
      </p>
      <p>Send this code to your invitee. Shown only now; the server keeps only its hash.</p>
      <code className="rounded-md bg-muted p-2 font-mono text-sm select-all">{created.code}</code>
      <p>{`Works once, until ${formatTimeWithZone(created.invite.expires_at)}.`}</p>
      <div className="flex flex-wrap gap-2">
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
    </div>
  );
}
