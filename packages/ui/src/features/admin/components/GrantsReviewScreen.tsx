// The Owner's grants review of one user (Plan.md P11.9; ADR-0018, ADR-0020, U79, S50, R178):
// the grants they hold first, dormant ones marked, computed by the server; then their accounts a
// grant shares, each account's grants read only when it's opened, since grants per account have
// no cap. Every revoke goes through the accounts' own route, asks first, and takes a step-up when
// the server wants one (useStepUpAction). The users' screen links here only for whom the Owner may
// review; anyone else who got here reads the server's refusal in place of each list.
// Nothing here carries a secret but the step-up's password, which StepUpForm holds. Every answer
// only reads the lists again (ADR-0020, S87).
import { useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { Fragment, useMemo, useState } from "react";
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
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import type { AccountResponse } from "../../../generated/AccountResponse";
import type { GrantResponse } from "../../../generated/GrantResponse";
import type { HeldGrantResponse } from "../../../generated/HeldGrantResponse";
import { formatTime } from "../../../lib/time";
import { accessLabel, revoked, revokeQuestion } from "../../accounts/texts";
import { useStepUpAction } from "../hooks/useStepUpAction";
import { StepUpPrompt } from "./StepUpPrompt";

const BACK_TO_USERS = "Back to users";
const HELD_HEADING = "Grants they hold";
const HELD_NONE = "They hold no grants.";
const DORMANT =
  "Dormant: their role already gives them Manage here. It counts again if they become a Member.";
const SHARED_HEADING = "Their accounts others can use";
const SHARED_NONE = "No grant shares their accounts.";
const SHOW_ACCESS = "Show who has access";
const HIDE_ACCESS = "Hide";

/** The screen's writes, as its rows take them. */
interface Revoking {
  /** Revokes `userId`'s grant on `account`, under `key`. */
  revoke(key: string, accountId: string, userId: string, who: string): void;
  readonly writes: ReturnType<typeof useStepUpAction>;
  readonly busy: boolean;
}

/** What a grant's row shows below itself: its question, its prompt, its refusal. */
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
  if (!asking && writes.prompt !== rowKey && failure === undefined) {
    return null;
  }
  return (
    <tr>
      <td colSpan={columns} className="pb-3">
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
        {writes.prompt === rowKey ? (
          <StepUpPrompt
            id={`step-up-${rowKey}`}
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
      variant="outline"
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
      <tr className="border-t align-top">
        <td className="py-2 pr-3">
          {grant.account.name}
          {grant.dormant ? (
            <span className="block text-xs text-muted-foreground">{DORMANT}</span>
          ) : null}
        </td>
        <td className="py-2 pr-3">{grant.owner.username}</td>
        <td className="py-2 pr-3">{accessLabel(grant.level)}</td>
        <td className="py-2 pr-3">{grant.granted_by.username}</td>
        <td className="py-2 pr-3">{formatTime(grant.granted_at)}</td>
        <td className="py-2 text-right">
          <RevokeButton
            who={who}
            account={grant.account.name}
            disabled={revoking.busy}
            onClick={() => setAsking(true)}
          />
        </td>
      </tr>
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

/** The grants they hold. */
function HeldSection({ id, who, revoking }: { id: string; who: string; revoking: Revoking }) {
  const held = useHeldGrants(id);
  const rows = held.data?.pages.flatMap((page) => page.items) ?? [];
  return (
    <section aria-labelledby="held-heading" className="flex flex-col gap-3">
      <h2 id="held-heading" className="text-lg font-semibold">
        {HELD_HEADING}
      </h2>
      {held.isError ? (
        <p role="alert" className="text-sm text-destructive">
          {describeError(toApiError(held.error).error)}
        </p>
      ) : null}
      {held.isSuccess && rows.length === 0 ? (
        <p className="text-muted-foreground">{HELD_NONE}</p>
      ) : null}
      {rows.length === 0 ? null : (
        <table aria-label={HELD_HEADING} className="w-full text-left text-sm">
          <thead className="text-muted-foreground">
            <tr>
              <th className="py-1 pr-3 font-medium">Account</th>
              <th className="py-1 pr-3 font-medium">Owner</th>
              <th className="py-1 pr-3 font-medium">Access</th>
              <th className="py-1 pr-3 font-medium">Given by</th>
              <th className="py-1 pr-3 font-medium">When</th>
              <th className="py-1 font-medium">
                <span className="sr-only">Actions</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {rows.map((grant) => (
              <HeldRow
                key={grant.account.id}
                grant={grant}
                who={who}
                userId={id}
                revoking={revoking}
              />
            ))}
          </tbody>
        </table>
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
    </section>
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
      <tr className="border-t align-top">
        <td className="py-2 pr-3">{who}</td>
        <td className="py-2 pr-3">{accessLabel(grant.level)}</td>
        <td className="py-2 pr-3">{grant.granted_by.username}</td>
        <td className="py-2 pr-3">{formatTime(grant.granted_at)}</td>
        <td className="py-2 text-right">
          <RevokeButton
            who={who}
            account={account.name}
            disabled={revoking.busy}
            onClick={() => setAsking(true)}
          />
        </td>
      </tr>
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
    <div className="flex flex-col gap-2 pl-4">
      {grants.isError ? (
        <p role="alert" className="text-sm text-destructive">
          {describeError(toApiError(grants.error).error)}
        </p>
      ) : null}
      {rows.length === 0 ? null : (
        <table aria-label={`Who has access to ${account.name}`} className="text-left text-sm">
          <thead className="text-muted-foreground">
            <tr>
              <th className="py-1 pr-3 font-medium">User</th>
              <th className="py-1 pr-3 font-medium">Access</th>
              <th className="py-1 pr-3 font-medium">Given by</th>
              <th className="py-1 pr-3 font-medium">When</th>
              <th className="py-1 font-medium">
                <span className="sr-only">Actions</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {rows.map((grant) => (
              <SharedGrantRow
                key={grant.user.id}
                account={account}
                grant={grant}
                revoking={revoking}
              />
            ))}
          </tbody>
        </table>
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
    <li className="flex flex-col gap-2 border-t py-2">
      <div className="flex items-center justify-between gap-2 text-sm">
        <span>{account.name}</span>
        <Button
          type="button"
          size="sm"
          variant="outline"
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
    <section aria-labelledby="shared-heading" className="flex flex-col gap-3">
      <h2 id="shared-heading" className="text-lg font-semibold">
        {SHARED_HEADING}
      </h2>
      {shared.isError ? (
        <p role="alert" className="text-sm text-destructive">
          {describeError(toApiError(shared.error).error)}
        </p>
      ) : null}
      {shared.isSuccess && rows.length === 0 ? (
        <p className="text-muted-foreground">{SHARED_NONE}</p>
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
    </section>
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
  const writes = useStepUpAction({ onRefused: refresh });
  const revoking: Revoking = {
    writes,
    busy: writes.running !== null || writes.prompt !== null,
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
    <main className="flex max-w-4xl flex-col gap-6 p-6">
      <h1 className="text-2xl font-semibold tracking-tight break-all">{`Grants of ${who}`}</h1>
      <HeldSection id={id} who={who} revoking={revoking} />
      <SharedSection id={id} revoking={revoking} />
      <Button asChild variant="link" className="self-start px-0">
        <Link to="/admin/users">{BACK_TO_USERS}</Link>
      </Button>
    </main>
  );
}
