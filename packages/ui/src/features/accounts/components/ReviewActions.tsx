// Approving or refusing a pending link (Plan.md P9.8, ADR-0018 groups A and E): offered only
// for the links the server lists as waiting for this user, each asking first, then "Confirm
// it's you" when the server wants the password. Refusing deletes the link.
import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { toast } from "sonner";

import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { ACCOUNTS_KEY } from "../../../api/queries";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import type { AccountResponse } from "../../../generated/AccountResponse";
import type { CommandError } from "../../../generated/CommandError";
import { StepUpPrompt } from "../../admin/components/StepUpPrompt";
import type { AccountWrites } from "../hooks/useAccountWrites";
import { approved, approveQuestion, NOT_WAITING, refused, refuseQuestion } from "../texts";

type Review = "approve" | "refuse";

/** A review's refusal: 409 means someone else reviewed it, or it's gone from pending. */
function describeReview(error: CommandError): string {
  return error.api?.status === 409 ? NOT_WAITING : describeError(error);
}

/** Approve and Refuse for `account`, with their questions, prompt and failure. */
export function ReviewActions({
  account,
  writes,
  onGone,
}: {
  account: AccountResponse;
  writes: AccountWrites;
  /** After a refusal went through: the account is gone. */
  onGone?: () => void;
}) {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const [asking, setAsking] = useState<Review | null>(null);
  const owner = account.owner?.username ?? "its owner";
  const key = `review:${account.id}`;

  function review(kind: Review) {
    setAsking(null);
    writes.attempt(
      key,
      async () => {
        if (kind === "approve") {
          await api.approveAccount(account.id);
          toast.success(approved(account.name));
        } else {
          await api.refuseAccount(account.id);
          toast.success(refused(account.name));
          onGone?.();
        }
        void queryClient.invalidateQueries({ queryKey: ACCOUNTS_KEY });
      },
      describeReview,
    );
  }

  return (
    <div className="flex flex-col gap-2">
      <div className="flex gap-2">
        <Button
          type="button"
          size="sm"
          aria-label={`Approve ${account.name}`}
          disabled={writes.busy}
          onClick={() => setAsking("approve")}
        >
          Approve
        </Button>
        <Button
          type="button"
          size="sm"
          variant="outline"
          aria-label={`Refuse ${account.name}`}
          disabled={writes.busy}
          onClick={() => setAsking("refuse")}
        >
          Refuse
        </Button>
      </div>
      {asking === null ? null : (
        <Alert>
          <AlertDescription className="flex flex-col gap-2 text-foreground">
            <p>
              {asking === "approve"
                ? approveQuestion(account.name, owner)
                : refuseQuestion(account.name, owner)}
            </p>
            <div className="flex gap-2">
              <Button
                type="button"
                variant={asking === "approve" ? "default" : "destructive"}
                disabled={writes.busy}
                onClick={() => review(asking)}
              >
                {asking === "approve" ? "Approve" : "Refuse"}
              </Button>
              <Button type="button" variant="outline" onClick={() => setAsking(null)}>
                Cancel
              </Button>
            </div>
          </AlertDescription>
        </Alert>
      )}
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
    </div>
  );
}
