// "Who has access" (Plan.md P9.8, P11.11, the user's U11 at group F's plan review): for a Manage
// holder, the account's grants with a level picker and Revoke, a page at a time, then
// "Share with" by exact username. No write asks for the password (ADR-0028);
// a level changes as soon as it's picked, a revoke asks first. The caller's own row
// offers neither: nobody grants to themselves, and Leave is their way out (ADR-0020, U83). The table
// never cuts text: narrow, it drops "When" first, then "Given by" (U109). A row's question
// and failure take a row of their own under it, only while there's one.

import { zodResolver } from "@hookform/resolvers/zod";
import { useQueryClient } from "@tanstack/react-query";
import { Fragment, useId, useState } from "react";
import { useForm } from "react-hook-form";
import { toast } from "sonner";
import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { ACCOUNTS_KEY, useGrants, useMe } from "../../../api/queries";
import { grantLevel, username } from "../../../api/schemas";
import { z } from "../../../api/zod";
import { FormField } from "../../../components/FormField";
import { NativeSelect } from "../../../components/NativeSelect";
import { RetryWaitMessage } from "../../../components/RetryWaitMessage";
import { ScreenState } from "../../../components/ScreenState";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import { Card } from "../../../components/ui/card";
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
import type { CommandError } from "../../../generated/CommandError";
import type { GrantLevel } from "../../../generated/GrantLevel";
import type { GrantResponse } from "../../../generated/GrantResponse";
import { useRetryWait } from "../../../hooks/useRetryWait";
import { showFieldErrors } from "../../../lib/formErrors";
import { formatTime } from "../../../lib/time";
import type { AccountWrites } from "../hooks/useAccountWrites";
import {
  ACCESS_HINTS,
  accessLabel,
  alreadyHasAccess,
  alreadyManages,
  LOADING_ACCESS,
  NOBODY_ELSE,
  nowHas,
  PENDING_NOT_SHARED,
  revoked,
  revokeQuestion,
  SHARE_HINT,
  shared,
  TOO_MANY_NAMES,
  YOU,
} from "../texts";
import { UsernameCombobox } from "./UsernameCombobox";

const LEVELS = ["view", "control", "manage"] as const;

/** The section for `account`, whose viewer manages it. */
export function AccessSection({
  account,
  writes,
}: {
  account: AccountResponse;
  writes: AccountWrites;
}) {
  const grants = useGrants(account.id, true);
  const rows = grants.data?.pages.flatMap((page) => page.items) ?? [];
  return (
    <section aria-labelledby="access-heading" className="flex min-w-0 flex-col gap-3">
      <h2 id="access-heading" className="text-base font-semibold">
        Who has access
      </h2>
      {grants.error === null ? null : (
        <ScreenState kind="error">{describeError(toApiError(grants.error).error)}</ScreenState>
      )}
      {grants.isPending ? <ScreenState kind="loading">{LOADING_ACCESS}</ScreenState> : null}
      {grants.isSuccess && rows.length === 0 ? (
        <ScreenState kind="empty">{NOBODY_ELSE}</ScreenState>
      ) : null}
      {rows.length === 0 ? null : (
        <Table aria-label="Who has access">
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
            {rows.map((row) => (
              <GrantRow key={row.user.id} account={account} row={row} writes={writes} />
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
      <ShareForm account={account} writes={writes} />
    </section>
  );
}

/** One grant, with its level picker, Revoke, and what they ask or answer. */
function GrantRow({
  account,
  row,
  writes,
}: {
  account: AccountResponse;
  row: GrantResponse;
  writes: AccountWrites;
}) {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const { data: user } = useMe();
  const mine = user?.id === row.user.id;
  const [asking, setAsking] = useState(false);
  const who = row.user.username;
  const key = `grant:${row.user.id}`;
  const refresh = () => void queryClient.invalidateQueries({ queryKey: ACCOUNTS_KEY });

  /** A change's 409: a pending account, or someone who manages it through their role. */
  function describeChange(error: CommandError): string {
    if (error.api?.status !== 409) {
      return describeError(error);
    }
    return account.status === "pending" ? PENDING_NOT_SHARED : alreadyManages(who, account.name);
  }

  function change(level: GrantLevel) {
    writes.attempt(
      key,
      async () => {
        await api.changeGrant(account.id, row.user.id, { level });
        toast.success(nowHas(who, level));
        refresh();
      },
      describeChange,
    );
  }

  function revoke() {
    setAsking(false);
    writes.attempt(key, async () => {
      await api.revokeGrant(account.id, row.user.id);
      toast.success(revoked(who));
      refresh();
    });
  }

  const failed = writes.failure?.key === key;
  return (
    <Fragment>
      <TableRow>
        <TableCell className="font-mono break-all">{who}</TableCell>
        <TableCell>
          {mine ? (
            accessLabel(row.level)
          ) : (
            <NativeSelect
              aria-label={`Access of ${who}`}
              value={row.level}
              disabled={writes.busy || user === undefined}
              onChange={(event) => change(event.target.value as GrantLevel)}
            >
              {LEVELS.map((level) => (
                <option key={level} value={level}>
                  {accessLabel(level)}
                </option>
              ))}
            </NativeSelect>
          )}
        </TableCell>
        <TableCell drop="under-520" className="font-mono break-all">
          {row.granted_by.username}
        </TableCell>
        <TableCell drop="under-620">{formatTime(row.granted_at)}</TableCell>
        <TableCell className="text-right">
          {mine ? YOU : null}
          {!mine && user !== undefined ? (
            <Button
              type="button"
              size="sm"
              variant="ghost"
              aria-label={`Revoke ${who}'s access`}
              disabled={writes.busy}
              onClick={() => setAsking(true)}
            >
              Revoke
            </Button>
          ) : null}
        </TableCell>
      </TableRow>
      {asking || failed ? (
        <TableSpanRow span={5}>
          <div className="flex flex-col gap-2">
            {asking ? (
              <Alert>
                <AlertDescription className="flex flex-col gap-2 text-foreground">
                  <p>{revokeQuestion(who, account.name)}</p>
                  <div className="flex gap-2">
                    <Button
                      type="button"
                      size="sm"
                      variant="destructive"
                      disabled={writes.busy}
                      onClick={revoke}
                    >
                      Revoke
                    </Button>
                    <Button
                      type="button"
                      size="sm"
                      variant="outline"
                      onClick={() => setAsking(false)}
                    >
                      Cancel
                    </Button>
                  </div>
                </AlertDescription>
              </Alert>
            ) : null}
            {failed ? (
              <p role="alert" className="text-sm text-destructive">
                {writes.failure?.message}
              </p>
            ) : null}
          </div>
        </TableSpanRow>
      ) : null}
    </Fragment>
  );
}

const shareForm = z.object({
  username: z
    .string()
    .refine((text) => text !== "", "Enter their username.")
    .pipe(username),
  level: grantLevel,
});

type ShareValues = z.infer<typeof shareForm>;

/** "Share with": a grant by exact username. */
function ShareForm({ account, writes }: { account: AccountResponse; writes: AccountWrites }) {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const form = useForm<ShareValues>({
    resolver: zodResolver(shareForm),
    defaultValues: { username: "", level: "view" },
  });
  const [formError, setFormError] = useState<string>();
  const wait = useRetryWait();
  const key = "share";
  const headingId = useId();
  const username = form.register("username");
  const typed = form.watch("username");

  /** What a refusal says, at the form. */
  function show(error: CommandError, user: string): void {
    const status = error.api?.status;
    const seconds = error.api?.retry_after_secs;
    if (status === 429 && seconds !== undefined && seconds > 0) {
      wait.startFor({ message: TOO_MANY_NAMES, seconds });
    } else if (status === 409) {
      setFormError(account.status === "pending" ? PENDING_NOT_SHARED : alreadyHasAccess(user));
    } else if (!showFieldErrors(form.setError, error, { username: "username" })) {
      setFormError(describeError(error));
    }
  }

  const submit = form.handleSubmit((values) => {
    if (wait.waiting !== null) {
      return;
    }
    setFormError(undefined);
    wait.reset();
    writes.attempt(key, async () => {
      try {
        await api.createGrant(account.id, values);
      } catch (rejection) {
        show(toApiError(rejection).error, values.username);
        return;
      }
      toast.success(shared(account.name, values.username));
      form.reset({ username: "", level: values.level });
      void queryClient.invalidateQueries({ queryKey: ACCOUNTS_KEY });
    });
  });

  return (
    <section aria-labelledby={headingId}>
      <Card className="gap-3 p-3">
        <h3 id={headingId} className="text-base font-semibold">
          Share with
        </h3>
        <form className="flex flex-col gap-4" noValidate onSubmit={(event) => void submit(event)}>
          <FormField
            id="share-username"
            label="Username"
            hint={SHARE_HINT}
            error={form.formState.errors.username?.message}
          >
            {(control) => (
              <UsernameCombobox
                {...control}
                name={username.name}
                onChange={username.onChange}
                onBlur={username.onBlur}
                inputRef={username.ref}
                account={account.id}
                searching={account.status !== "pending"}
                value={typed}
                onPick={(name) => form.setValue("username", name)}
              />
            )}
          </FormField>
          <fieldset className="flex flex-col gap-2">
            <legend className="text-sm font-semibold">Access</legend>
            {LEVELS.map((level) => (
              <label key={level} className="flex items-start gap-2 text-sm">
                <input type="radio" value={level} {...form.register("level")} className="mt-1" />
                <span>
                  {accessLabel(level)}
                  <span className="block text-xs text-muted-foreground">{ACCESS_HINTS[level]}</span>
                </span>
              </label>
            ))}
          </fieldset>
          {formError === undefined ? null : (
            <p role="alert" className="text-sm text-destructive">
              {formError}
            </p>
          )}
          <RetryWaitMessage wait={wait} />
          <Button
            type="submit"
            className="self-start"
            disabled={writes.busy || wait.waiting !== null}
          >
            Share
          </Button>
        </form>
      </Card>
    </section>
  );
}
