// One account's screen (Plan.md P9.8, the user's U11 at group F's plan review): its facts as
// the viewer may see them, a pending link's note or review, the re-auth prompt, "Who has
// access" for a Manage holder, then Link again and Delete. Deleting asks first, then "Confirm
// it's you" when the server wants it, and goes back to the list.
import { useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { toast } from "sonner";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { ACCOUNTS_KEY, useAccount, useMe } from "../../../api/queries";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import { formatTime } from "../../../lib/time";
import { StepUpPrompt } from "../../admin/components/StepUpPrompt";
import { useAccountWrites } from "../hooks/useAccountWrites";
import { useAwaitingApproval } from "../hooks/useAwaitingApproval";
import { accountActions } from "../permissions";
import {
  accessLabel,
  deleted,
  deleteQuestion,
  kindLabel,
  NOT_FOUND,
  PENDING_NOTE,
  statusLabel,
} from "../texts";
import { AccessSection } from "./AccessSection";
import { ReauthPrompt } from "./ReauthPrompt";
import { ReviewActions } from "./ReviewActions";

/** Account `id`. */
export function AccountScreen({ id }: { id: string }) {
  const account = useAccount(id);
  const { data: user } = useMe();
  const { query: awaiting } = useAwaitingApproval();
  const writes = useAccountWrites();
  const navigate = useNavigate();

  // A read that failed shows instead of the account only before it was ever read, or when
  // it answers 404: the account went while its screen was open (P11.8, S60).
  const failure = account.error === null ? null : toApiError(account.error).error;
  if (failure !== null && (account.data === undefined || failure.api?.status === 404)) {
    const error = failure;
    return (
      <main className="flex max-w-3xl flex-col gap-4 p-6">
        <p role="alert">{error.api?.status === 404 ? NOT_FOUND : describeError(error)}</p>
        <BackLink />
      </main>
    );
  }
  const data = account.data;
  if (data === undefined) {
    return <main className="p-6" />;
  }
  const actions = accountActions(data);
  const reviewable = (awaiting.data ?? []).some((item) => item.id === data.id);
  const facts: [string, string][] = [
    ["Type", kindLabel(data.kind)],
    ["Status", statusLabel(data.status)],
    ["Your access", accessLabel(data.level)],
    ...(data.owner === undefined
      ? []
      : ([["Linked by", data.owner.id === user?.id ? "you" : data.owner.username]] as [
          string,
          string,
        ][])),
    ...(data.linked_at === undefined
      ? []
      : ([["Linked on", formatTime(data.linked_at)]] as [string, string][])),
  ];
  return (
    <main className="flex max-w-3xl flex-col gap-6 p-6">
      <h1 className="text-2xl font-semibold tracking-tight">{data.name}</h1>
      <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 text-sm">
        {facts.map(([term, value]) => (
          <div key={term} className="contents">
            <dt className="text-muted-foreground">{term}</dt>
            <dd>{value}</dd>
          </div>
        ))}
      </dl>
      {data.status === "pending" && !reviewable ? <p>{PENDING_NOTE}</p> : null}
      {reviewable ? (
        <ReviewActions
          account={data}
          writes={writes}
          onGone={() => void navigate({ to: "/accounts" })}
        />
      ) : null}
      <ReauthPrompt account={data} canLinkAgain={actions.linkAgain} place="screen" />
      {actions.share ? <AccessSection account={data} writes={writes} /> : null}
      <div className="flex flex-wrap gap-2">
        {actions.linkAgain && data.status === "active" ? (
          <Button asChild variant="outline">
            <Link to="/accounts/$id/link-again" params={{ id: data.id }}>
              Link again
            </Link>
          </Button>
        ) : null}
      </div>
      {actions.delete ? <DeleteAction id={data.id} name={data.name} writes={writes} /> : null}
      <BackLink />
    </main>
  );
}

/** "Back to accounts". */
function BackLink() {
  return (
    <Button asChild variant="link" className="self-start px-0">
      <Link to="/accounts">Back to accounts</Link>
    </Button>
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
  const [asking, setAsking] = useState(false);
  const key = "delete";

  function remove() {
    setAsking(false);
    writes.attempt(key, async () => {
      await api.deleteAccount(id);
      toast.success(deleted(name));
      void queryClient.invalidateQueries({ queryKey: ACCOUNTS_KEY });
      void navigate({ to: "/accounts" });
    });
  }

  return (
    <div className="flex flex-col gap-2">
      <Button
        type="button"
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
