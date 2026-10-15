// "Who has access" (Plan.md P9.8, the user's U11 at group F's plan review): for a Manage
// holder, the account's grants with a level picker and Revoke, a page at a time, then
// "Share with" by exact username. Every write may ask for the password first (ADR-0018
// group E); a level changes as soon as it's picked, a revoke asks first.
import { zodResolver } from "@hookform/resolvers/zod";
import { useQueryClient } from "@tanstack/react-query";
import { Fragment, useState } from "react";
import { useForm } from "react-hook-form";
import { toast } from "sonner";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { ACCOUNTS_KEY, useGrants } from "../../../api/queries";
import { grantLevel, username } from "../../../api/schemas";
import { z } from "../../../api/zod";
import { FormField } from "../../../components/FormField";
import { RetryWaitMessage } from "../../../components/RetryWaitMessage";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import type { AccountResponse } from "../../../generated/AccountResponse";
import type { CommandError } from "../../../generated/CommandError";
import type { GrantLevel } from "../../../generated/GrantLevel";
import type { GrantResponse } from "../../../generated/GrantResponse";
import { useRetryWait } from "../../../hooks/useRetryWait";
import { showFieldErrors } from "../../../lib/formErrors";
import { isStepUpRequired } from "../../../lib/stepUp";
import { formatTime } from "../../../lib/time";
import { StepUpPrompt } from "../../admin/components/StepUpPrompt";
import type { AccountWrites } from "../hooks/useAccountWrites";
import {
  ACCESS_HINTS,
  accessLabel,
  alreadyHasAccess,
  alreadyManages,
  NOBODY_ELSE,
  nowHas,
  PENDING_NOT_SHARED,
  revoked,
  revokeQuestion,
  shared,
  TOO_MANY_NAMES,
} from "../texts";

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
    <section aria-labelledby="access-heading" className="flex flex-col gap-3">
      <h2 id="access-heading" className="text-lg font-semibold">
        Who has access
      </h2>
      {grants.error === null ? null : (
        <p role="alert" className="text-sm text-destructive">
          {describeError(toApiError(grants.error).error)}
        </p>
      )}
      {grants.isSuccess && rows.length === 0 ? (
        <p className="text-muted-foreground">{NOBODY_ELSE}</p>
      ) : (
        <table aria-label="Who has access" className="w-full text-left text-sm">
          <thead>
            <tr className="border-b">
              <th className="py-2 font-medium">User</th>
              <th className="py-2 font-medium">Access</th>
              <th className="py-2 font-medium">Given by</th>
              <th className="py-2 font-medium">When</th>
              <th className="py-2">
                <span className="sr-only">Actions</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <GrantRow key={row.user.id} account={account} row={row} writes={writes} />
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

  return (
    <Fragment>
      <tr className="border-b">
        <td className="py-2">{who}</td>
        <td className="py-2">
          <select
            aria-label={`Access of ${who}`}
            className="rounded-md border bg-background px-2 py-1"
            value={row.level}
            disabled={writes.busy}
            onChange={(event) => change(event.target.value as GrantLevel)}
          >
            {LEVELS.map((level) => (
              <option key={level} value={level}>
                {accessLabel(level)}
              </option>
            ))}
          </select>
        </td>
        <td className="py-2">{row.granted_by.username}</td>
        <td className="py-2">{formatTime(row.granted_at)}</td>
        <td className="py-2 text-right">
          <Button
            type="button"
            size="sm"
            variant="outline"
            aria-label={`Revoke ${who}'s access`}
            disabled={writes.busy}
            onClick={() => setAsking(true)}
          >
            Revoke
          </Button>
        </td>
      </tr>
      <tr>
        <td colSpan={5} className="pb-2">
          {asking ? (
            <Alert>
              <AlertDescription className="flex flex-col gap-2 text-foreground">
                <p>{revokeQuestion(who, account.name)}</p>
                <div className="flex gap-2">
                  <Button
                    type="button"
                    variant="destructive"
                    disabled={writes.busy}
                    onClick={revoke}
                  >
                    Revoke
                  </Button>
                  <Button type="button" variant="outline" onClick={() => setAsking(false)}>
                    Cancel
                  </Button>
                </div>
              </AlertDescription>
            </Alert>
          ) : null}
          {writes.prompt === key ? (
            <StepUpPrompt
              id={`step-up-${key}`}
              pending={writes.running !== null}
              onConfirm={writes.confirm}
              onCancel={writes.cancel}
            />
          ) : null}
          {writes.failure?.key === key ? (
            <p role="alert" className="text-sm text-destructive">
              {writes.failure.message}
            </p>
          ) : null}
        </td>
      </tr>
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

  /** What a refusal other than the step-up's says, at the form. */
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
        const error = toApiError(rejection).error;
        if (isStepUpRequired(error)) {
          throw rejection;
        }
        show(error, values.username);
        return;
      }
      toast.success(shared(account.name, values.username));
      form.reset({ username: "", level: values.level });
      void queryClient.invalidateQueries({ queryKey: ACCOUNTS_KEY });
    });
  });

  return (
    <section aria-labelledby="share-heading" className="flex flex-col gap-3">
      <h3 id="share-heading" className="font-semibold">
        Share with
      </h3>
      <form className="flex flex-col gap-4" noValidate onSubmit={(event) => void submit(event)}>
        <FormField
          id="share-username"
          label="Username"
          hint="Their exact username."
          error={form.formState.errors.username?.message}
        >
          {(control) => <Input {...control} autoComplete="off" {...form.register("username")} />}
        </FormField>
        <fieldset className="flex flex-col gap-2">
          <legend className="text-sm font-medium">Access</legend>
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
      {/* Outside the form: "Confirm it's you" is a form of its own, and a form inside a form
          submits the outer one natively, which reloads the app. */}
      {writes.prompt === key ? (
        <StepUpPrompt
          id={`step-up-${key}`}
          pending={writes.running !== null}
          onConfirm={writes.confirm}
          onCancel={writes.cancel}
        />
      ) : null}
      {writes.failure?.key === key ? (
        <p role="alert" className="text-sm text-destructive">
          {writes.failure.message}
        </p>
      ) : null}
    </section>
  );
}
