// One account's screen (Plan.md P9.8, P11.11, the user's U11 at group F's plan review; ADR-0020, U109):
// on the left, a box with its name, status and facts as the viewer may see them, a pending link's note
// or review, the re-auth prompt and Link again, and at its foot, set apart, Delete, and Leave where the
// server says the viewer may (U23, S51, U83); on the right, for a Manage holder, "Who has access". Two
// columns as a bot's screen has, stacking at the same width; one below Manage, never an empty right
// half. Deleting and leaving ask first, then "Confirm it's you" when the
// server wants it, and go back to the list, replacing this screen's entry, as it's gone (U111); a late
// answer moves nothing once the user has gone elsewhere.
import { useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { cn } from "cn";
import { useId, useState } from "react";
import { toast } from "sonner";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { ACCOUNTS_KEY, BOTS_LIST_KEY, useAccount, useMe } from "../../../api/queries";
import { type Fact, Facts } from "../../../components/Facts";
import { ScreenState } from "../../../components/ScreenState";
import { StatusLine } from "../../../components/StatusLine";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import { Card } from "../../../components/ui/card";
import { useAlive } from "../../../hooks/useStepGuard";
import { isStepUpRequired } from "../../../lib/stepUp";
import { formatTime } from "../../../lib/time";
import { StepUpPrompt } from "../../admin/components/StepUpPrompt";
import { useAccountWrites } from "../hooks/useAccountWrites";
import { useAwaitingApproval } from "../hooks/useAwaitingApproval";
import { accountActions } from "../permissions";
import {
  ACCOUNT_LABEL,
  accessLabel,
  accountLine,
  alreadyGone,
  deleted,
  deleteQuestion,
  kindLabel,
  LOADING_ACCOUNT,
  leaveQuestion,
  left,
  NOT_FOUND,
  PENDING_NOTE,
} from "../texts";
import { AccessSection } from "./AccessSection";
import { linkedByName } from "./AccountCard";
import { ReauthPrompt } from "./ReauthPrompt";
import { ReviewActions } from "./ReviewActions";

/** Account `id`. */
export function AccountScreen({ id }: { id: string }) {
  const account = useAccount(id);
  const { data: user } = useMe();
  const { query: awaiting } = useAwaitingApproval();
  const writes = useAccountWrites();
  const navigate = useNavigate();
  const nameId = useId();

  // A read that failed shows instead of the account only before it was ever read, or when
  // it answers 404: the account went while its screen was open (P11.8, S60).
  const failure = account.error === null ? null : toApiError(account.error).error;
  if (failure !== null && (account.data === undefined || failure.api?.status === 404)) {
    const error = failure;
    return (
      <main className="flex max-w-3xl flex-col gap-4 p-4">
        <ScreenState kind="error">
          {error.api?.status === 404 ? NOT_FOUND : describeError(error)}
        </ScreenState>
      </main>
    );
  }
  const data = account.data;
  if (data === undefined) {
    return (
      <main className="flex max-w-3xl flex-col gap-4 p-4">
        <ScreenState kind="loading">{LOADING_ACCOUNT}</ScreenState>
      </main>
    );
  }
  const actions = accountActions(data);
  const reviewable = (awaiting.data ?? []).some((item) => item.id === data.id);
  const linked = linkedByName(data, user?.id);
  const facts: Fact[] = [
    { term: ACCOUNT_LABEL.type, value: kindLabel(data.kind) },
    { term: ACCOUNT_LABEL.access, value: accessLabel(data.level) },
    ...(linked === undefined ? [] : [{ term: ACCOUNT_LABEL.linkedBy, value: linked }]),
    ...(data.linked_at === undefined
      ? []
      : [{ term: ACCOUNT_LABEL.linkedOn, value: formatTime(data.linked_at) }]),
  ];
  const line = accountLine(data.status);
  return (
    <main className={cn("flex flex-col p-4", actions.share ? undefined : "max-w-2xl")}>
      <div
        data-slot="account-columns"
        className={cn(
          "grid items-start gap-3",
          actions.share && "@min-[53rem]/content:grid-cols-[minmax(18rem,2fr)_3fr]",
        )}
      >
        <section aria-labelledby={nameId} className="min-w-0">
          <Card className="gap-3 p-3">
            <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1">
              <h1 id={nameId} className="font-mono text-lg font-semibold break-all">
                {data.name}
              </h1>
              <StatusLine tone={line.tone}>{line.text}</StatusLine>
            </div>
            <Facts facts={facts} place="screen" />
            {data.status === "pending" && !reviewable ? (
              <p className="text-sm">{PENDING_NOTE}</p>
            ) : null}
            {reviewable ? (
              <ReviewActions
                account={data}
                writes={writes}
                onGone={() => void navigate({ to: "/accounts", replace: true })}
              />
            ) : null}
            <ReauthPrompt account={data} canLinkAgain={actions.linkAgain} place="screen" />
            {actions.linkAgain && data.status === "active" ? (
              <Button asChild size="sm" variant="outline" className="self-start">
                <Link to="/accounts/$id/link-again" params={{ id: data.id }}>
                  Link again
                </Link>
              </Button>
            ) : null}
            {actions.delete || actions.leave ? (
              <div data-slot="account-foot" className="flex flex-col gap-3 border-t pt-3">
                {actions.delete ? (
                  <DeleteAction id={data.id} name={data.name} writes={writes} />
                ) : null}
                {actions.leave ? (
                  <LeaveAction id={data.id} name={data.name} writes={writes} />
                ) : null}
              </div>
            ) : null}
          </Card>
        </section>
        {actions.share ? <AccessSection account={data} writes={writes} /> : null}
      </div>
    </main>
  );
}

/** Delete, with its question, prompt and failure. */
function DeleteAction({
  id,
  name,
  writes,
}: {
  id: string;
  name: string;
  writes: ReturnType<typeof useAccountWrites>;
}) {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const alive = useAlive();
  const [asking, setAsking] = useState(false);
  const key = "delete";

  function remove() {
    setAsking(false);
    writes.attempt(key, async () => {
      await api.deleteAccount(id);
      toast.success(deleted(name));
      void queryClient.invalidateQueries({ queryKey: ACCOUNTS_KEY });
      if (alive.current) {
        void navigate({ to: "/accounts", replace: true });
      }
    });
  }

  return (
    <div className="flex flex-col gap-2">
      <Button
        type="button"
        size="sm"
        variant="destructive"
        className="self-start"
        disabled={writes.busy}
        onClick={() => setAsking(true)}
      >
        Delete
      </Button>
      {asking ? (
        <Alert>
          <AlertDescription className="flex flex-col gap-2 text-foreground">
            <p>{deleteQuestion(name)}</p>
            <div className="flex gap-2">
              <Button type="button" variant="destructive" disabled={writes.busy} onClick={remove}>
                Delete account
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
          id="step-up-delete"
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
    </div>
  );
}

/** Leave, with its question, prompt and failure. */
function LeaveAction({
  id,
  name,
  writes,
}: {
  id: string;
  name: string;
  writes: ReturnType<typeof useAccountWrites>;
}) {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const alive = useAlive();
  const [asking, setAsking] = useState(false);
  const key = "leave";

  /** Goes back first, so the account's own read never answers its 404 on this screen (R179). */
  async function goBack(): Promise<void> {
    if (alive.current) {
      await navigate({ to: "/accounts", replace: true });
    }
    void queryClient.invalidateQueries({ queryKey: ACCOUNTS_KEY });
    void queryClient.invalidateQueries({ queryKey: BOTS_LIST_KEY });
  }

  function leave() {
    setAsking(false);
    writes.attempt(key, async () => {
      try {
        await api.leaveAccount(id);
      } catch (rejection) {
        const error = toApiError(rejection).error;
        if (isStepUpRequired(error) || error.api?.status !== 404) {
          throw rejection;
        }
        toast.info(alreadyGone(name));
        await goBack();
        return;
      }
      toast.success(left(name));
      await goBack();
    });
  }

  return (
    <div className="flex flex-col gap-2">
      <Button
        type="button"
        size="sm"
        variant="outline"
        className="self-start"
        disabled={writes.busy}
        onClick={() => setAsking(true)}
      >
        Leave
      </Button>
      {asking ? (
        <Alert>
          <AlertDescription className="flex flex-col gap-2 text-foreground">
            <p>{leaveQuestion(name)}</p>
            <div className="flex gap-2">
              <Button type="button" variant="destructive" disabled={writes.busy} onClick={leave}>
                Leave account
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
          id="step-up-leave"
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
    </div>
  );
}
