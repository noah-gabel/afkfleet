// The Owner's grants review of one user (Plan.md P11.9, P11.11; ADR-0018, ADR-0020, U79, U124, S50,
// S90, R178): the grants they hold, dormant ones marked, computed by the server, beside their
// accounts a grant shares, each account's grants read only when it's opened, since grants per
// account have no cap, and dormant ones marked there too. Side by side from an 830 px box, the held
// grants first when stacked: under the 833 px the column holds beside the wide sidebar with its
// scrollbar, as this page scrolls in both forms, so the wide sidebar never stacks it again (PR #92's
// review). Each table drops When, then Given by, in a small box (U122).
// Every revoke goes through the accounts' own route and asks first, never for the password
// (useWriteAction, ADR-0028). The users' screen links here only for whom the Owner may review;
// anyone else who got here reads the server's refusal in place of each list.
// Nothing here carries a secret. Every answer only reads the lists again (ADR-0020, S87).
import { useQueryClient } from "@tanstack/react-query";
import { Fragment, type ReactNode, useMemo, useState } from "react";
import { toast } from "sonner";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import {
  ACCOUNTS_KEY,
  USERS_KEY,
  useGrants,
  useHeldGrants,
  useSharedAccounts,
  useUserNames,
} from "../../../api/queries";
import { ScreenState } from "../../../components/ScreenState";
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
import type { AccountResponse } from "../../../generated/AccountResponse";
import type { GrantResponse } from "../../../generated/GrantResponse";
import type { HeldGrantResponse } from "../../../generated/HeldGrantResponse";
import { formatTime } from "../../../lib/time";
import {
  accessLabel,
  LOADING_ACCESS,
  NOBODY_ELSE,
  revoked,
  revokeQuestion,
} from "../../accounts/texts";
import { useWriteAction } from "../hooks/useWriteAction";

const HELD_HEADING = "Grants they hold";
const HELD_NONE = "No grants held.";
const LOADING_HELD = "Loading grants…";
const DORMANT =
  "Dormant: their role gives Manage here. Counts again if they become a Member or the owner an Admin.";
const SHARED_HEADING = "Their accounts others can use";
const SHARED_NONE = "None shared.";
const LOADING_SHARED = "Loading accounts…";
const SHOW_ACCESS = "Show who has access";
const HIDE_ACCESS = "Hide";

/** Why the held grants couldn't be read. */
function heldFailed(why: string): string {
  return `Couldn't load grants: ${why}`;
}

/** Why the shared accounts couldn't be read. */
function sharedFailed(why: string): string {
  return `Couldn't load accounts: ${why}`;
}

/** The mark on a grant on the reviewed user's account: "their" would read as the reviewed user. */
function dormantOn(who: string): string {
  return `Dormant: ${who}'s role gives Manage here. Counts again if ${who} becomes a Member or the owner an Admin.`;
}

/** The screen's writes, as its rows take them. */
interface Revoking {
  /** Revokes `userId`'s grant on `account`, under `key`. */
  revoke(key: string, accountId: string, userId: string, who: string): void;
  readonly writes: ReturnType<typeof useWriteAction>;
  readonly busy: boolean;
}

/** What a grant's row shows below itself: its question, its refusal. */
function RevokeExtras({
  revoking,
  rowKey,
  asking,
  question,
  onConfirm,
  onCancel,
  columns,
}: {
  revoking: Revoking;
  rowKey: string;
  asking: boolean;
  question: string;
  onConfirm: () => void;
  onCancel: () => void;
  columns: number;
}) {
  const { writes, busy } = revoking;
  const failure = writes.failure?.key === rowKey ? writes.failure.message : undefined;
  if (!asking && failure === undefined) {
    return null;
  }
  return (
    <TableSpanRow span={columns} className="pb-3">
      <div className="flex flex-col gap-2">
        {asking ? (
          <Alert>
            <AlertDescription className="flex flex-col gap-2 text-foreground">
              <p>{question}</p>
              <div className="flex gap-2">
                <Button type="button" variant="destructive" disabled={busy} onClick={onConfirm}>
                  Revoke
                </Button>
                <Button type="button" variant="outline" onClick={onCancel}>
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
  );
}

/** A grant's Revoke, naming whose and on what for screen readers. */
function RevokeButton({
  who,
  account,
  disabled,
  onClick,
}: {
  who: string;
  account: string;
  disabled: boolean;
  onClick: () => void;
}) {
  return (
    <Button
      type="button"
      size="sm"
      variant="ghost"
      aria-label={`Revoke ${who}'s access to ${account}`}
      disabled={disabled}
      onClick={onClick}
    >
      Revoke
    </Button>
  );
}

/** One grant the user holds. */
function HeldRow({
  grant,
  who,
  userId,
  revoking,
}: {
  grant: HeldGrantResponse;
  who: string;
  userId: string;
  revoking: Revoking;
}) {
  const key = `held:${grant.account.id}`;
  const [asking, setAsking] = useState(false);
  return (
    <Fragment>
      <TableRow>
        <TableCell>
          <span className="font-mono break-all">{grant.account.name}</span>
          {grant.dormant ? (
            <span className="block text-xs text-muted-foreground">{DORMANT}</span>
          ) : null}
        </TableCell>
        <TableCell className="font-mono break-all">{grant.owner.username}</TableCell>
        <TableCell>{accessLabel(grant.level)}</TableCell>
        <TableCell drop="under-520" className="font-mono break-all">
          {grant.granted_by.username}
        </TableCell>
        <TableCell drop="under-620">{formatTime(grant.granted_at)}</TableCell>
        <TableCell className="text-right">
          <RevokeButton
            who={who}
            account={grant.account.name}
            disabled={revoking.busy}
            onClick={() => setAsking(true)}
          />
        </TableCell>
      </TableRow>
      <RevokeExtras
        revoking={revoking}
        rowKey={key}
        asking={asking}
        question={revokeQuestion(who, grant.account.name)}
        onConfirm={() => {
          setAsking(false);
          revoking.revoke(key, grant.account.id, userId, who);
        }}
        onCancel={() => setAsking(false)}
        columns={6}
      />
    </Fragment>
  );
}

/** One of the review's two sections: its heading in a box, then its list. */
function Section({ id, heading, children }: { id: string; heading: string; children: ReactNode }) {
  return (
    <section
      aria-labelledby={id}
      className="flex min-w-0 flex-col gap-3 rounded-lg border bg-card p-3"
    >
      <h2 id={id} className="text-base font-semibold">
        {heading}
      </h2>
      {children}
    </section>
  );
}

/** The grants they hold. */
function HeldSection({ id, who, revoking }: { id: string; who: string; revoking: Revoking }) {
  const held = useHeldGrants(id);
  const rows = held.data?.pages.flatMap((page) => page.items) ?? [];
  return (
    <Section id="held-heading" heading={HELD_HEADING}>
      {held.isError ? (
        <ScreenState kind="error">
          {heldFailed(describeError(toApiError(held.error).error))}
        </ScreenState>
      ) : null}
      {held.isPending ? <ScreenState kind="loading">{LOADING_HELD}</ScreenState> : null}
      {held.isSuccess && rows.length === 0 ? (
        <ScreenState kind="empty">{HELD_NONE}</ScreenState>
      ) : null}
      {rows.length === 0 ? null : (
        <Table aria-label={HELD_HEADING}>
          <TableHeader>
            <TableRow>
              <TableHead>Account</TableHead>
              <TableHead>Owner</TableHead>
              <TableHead>Access</TableHead>
              <TableHead drop="under-520">Given by</TableHead>
              <TableHead drop="under-620">When</TableHead>
              <TableHead>
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((grant) => (
              <HeldRow
                key={grant.account.id}
                grant={grant}
                who={who}
                userId={id}
                revoking={revoking}
              />
            ))}
          </TableBody>
        </Table>
      )}
      {held.hasNextPage ? (
        <Button
          type="button"
          variant="outline"
          className="self-start"
          disabled={held.isFetchingNextPage}
          onClick={() => void held.fetchNextPage()}
        >
          Show more
        </Button>
      ) : null}
    </Section>
  );
}

/** One grant on a shared account. */
function SharedGrantRow({
  account,
  grant,
  revoking,
}: {
  account: AccountResponse;
  grant: GrantResponse;
  revoking: Revoking;
}) {
  const who = grant.user.username;
  const key = `shared:${account.id}:${grant.user.id}`;
  const [asking, setAsking] = useState(false);
  return (
    <Fragment>
      <TableRow>
        <TableCell>
          <span className="font-mono break-all">{who}</span>
          {grant.dormant === true ? (
            <span className="block text-xs text-muted-foreground">{dormantOn(who)}</span>
          ) : null}
        </TableCell>
        <TableCell>{accessLabel(grant.level)}</TableCell>
        <TableCell drop="under-520" className="font-mono break-all">
          {grant.granted_by.username}
        </TableCell>
        <TableCell drop="under-620">{formatTime(grant.granted_at)}</TableCell>
        <TableCell className="text-right">
          <RevokeButton
            who={who}
            account={account.name}
            disabled={revoking.busy}
            onClick={() => setAsking(true)}
          />
        </TableCell>
      </TableRow>
      <RevokeExtras
        revoking={revoking}
        rowKey={key}
        asking={asking}
        question={revokeQuestion(who, account.name)}
        onConfirm={() => {
          setAsking(false);
          revoking.revoke(key, account.id, grant.user.id, who);
        }}
        onCancel={() => setAsking(false)}
        columns={5}
      />
    </Fragment>
  );
}

/** An opened account's grants, read now. */
function SharedGrants({ account, revoking }: { account: AccountResponse; revoking: Revoking }) {
  const grants = useGrants(account.id, true);
  const rows = grants.data?.pages.flatMap((page) => page.items) ?? [];
  return (
    <div className="flex flex-col gap-2">
      {grants.isError ? (
        <ScreenState kind="error">{describeError(toApiError(grants.error).error)}</ScreenState>
      ) : null}
      {grants.isPending ? <ScreenState kind="loading">{LOADING_ACCESS}</ScreenState> : null}
      {grants.isSuccess && rows.length === 0 ? (
        <ScreenState kind="empty">{NOBODY_ELSE}</ScreenState>
      ) : null}
      {rows.length === 0 ? null : (
        <Table aria-label={`Who has access to ${account.name}`}>
          <TableHeader>
            <TableRow>
              <TableHead>User</TableHead>
              <TableHead>Access</TableHead>
              <TableHead drop="under-520">Given by</TableHead>
              <TableHead drop="under-620">When</TableHead>
              <TableHead>
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((grant) => (
              <SharedGrantRow
                key={grant.user.id}
                account={account}
                grant={grant}
                revoking={revoking}
              />
            ))}
          </TableBody>
        </Table>
      )}
      {grants.hasNextPage ? (
        <Button
          type="button"
          variant="outline"
          className="self-start"
          disabled={grants.isFetchingNextPage}
          onClick={() => void grants.fetchNextPage()}
        >
          Show more
        </Button>
      ) : null}
    </div>
  );
}

/** One of their accounts a grant shares, its grants read only when it's opened. */
function SharedAccount({ account, revoking }: { account: AccountResponse; revoking: Revoking }) {
  const [open, setOpen] = useState(false);
  return (
    <li className="flex flex-col gap-2 border-t py-2 first:border-t-0 first:pt-0">
      <div className="flex items-center justify-between gap-2 text-sm">
        <span className="min-w-0 font-mono break-all">{account.name}</span>
        <Button
          type="button"
          size="sm"
          variant="ghost"
          aria-label={`${open ? "Hide" : "Show"} who has access to ${account.name}`}
          onClick={() => setOpen(!open)}
        >
          {open ? HIDE_ACCESS : SHOW_ACCESS}
        </Button>
      </div>
      {open ? <SharedGrants account={account} revoking={revoking} /> : null}
    </li>
  );
}

/** Their accounts a grant shares. */
function SharedSection({ id, revoking }: { id: string; revoking: Revoking }) {
  const shared = useSharedAccounts(id);
  const rows = shared.data?.pages.flatMap((page) => page.items) ?? [];
  return (
    <Section id="shared-heading" heading={SHARED_HEADING}>
      {shared.isError ? (
        <ScreenState kind="error">
          {sharedFailed(describeError(toApiError(shared.error).error))}
        </ScreenState>
      ) : null}
      {shared.isPending ? <ScreenState kind="loading">{LOADING_SHARED}</ScreenState> : null}
      {shared.isSuccess && rows.length === 0 ? (
        <ScreenState kind="empty">{SHARED_NONE}</ScreenState>
      ) : null}
      {rows.length === 0 ? null : (
        <ul className="flex flex-col">
          {rows.map((account) => (
            <SharedAccount key={account.id} account={account} revoking={revoking} />
          ))}
        </ul>
      )}
      {shared.hasNextPage ? (
        <Button
          type="button"
          variant="outline"
          className="self-start"
          disabled={shared.isFetchingNextPage}
          onClick={() => void shared.fetchNextPage()}
        >
          Show more
        </Button>
      ) : null}
    </Section>
  );
}

/** `/admin/users/$id/grants`. */
export function GrantsReviewScreen({ id }: { id: string }) {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const names = useUserNames();
  const who = useMemo(
    () => names.data?.find((user) => user.id === id)?.username ?? id,
    [names.data, id],
  );
  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: USERS_KEY });
    void queryClient.invalidateQueries({ queryKey: ACCOUNTS_KEY });
  };
  const writes = useWriteAction({ onRefused: refresh });
  const revoking: Revoking = {
    writes,
    busy: writes.running !== null,
    revoke(key, accountId, userId, name) {
      void writes.attempt({
        key,
        run: async () => {
          await api.revokeGrant(accountId, userId);
          toast.success(revoked(name));
          refresh();
        },
      });
    },
  };
  return (
    <main className="flex flex-1 flex-col gap-4 p-4">
      <h1 className="text-lg font-semibold tracking-tight break-all">{`Grants of ${who}`}</h1>
      <div
        data-slot="review-sections"
        className="grid items-start gap-3 @min-[51.875rem]/content:grid-cols-2"
      >
        <HeldSection id={id} who={who} revoking={revoking} />
        <SharedSection id={id} revoking={revoking} />
      </div>
    </main>
  );
}
