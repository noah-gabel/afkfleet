// A mode's Delete (Plan.md P11.9; ADR-0020, U66, S16, S17, R162): shown from the server's flag, it
// asks first, then goes straight through `useStepUpAction`; afterwards, back to the list. A
// refusal shows in place, and the mode stays.
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { toast } from "sonner";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import type { ModeResponse } from "../../../generated/ModeResponse";
import { useAlive } from "../../../hooks/useStepGuard";
import { isStepUpRequired } from "../../../lib/stepUp";
import { StepUpPrompt } from "../../admin/components/StepUpPrompt";
import type { useStepUpAction } from "../../admin/hooks/useStepUpAction";
import { forgetMode } from "../cache";
import { CANCEL, DELETE, DELETE_MODE, deleted, deleteQuestion } from "../texts";

/** The write's key: one at a time per screen. */
const DELETE_KEY = "delete";

/** Delete, with its question, prompt and failure; `onGone` when the mode isn't there any more. */
export function DeleteMode({
  mode,
  writes,
  onGone,
}: {
  mode: ModeResponse;
  writes: ReturnType<typeof useStepUpAction>;
  onGone: () => void;
}) {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const alive = useAlive();
  const [asking, setAsking] = useState(false);
  const [failure, setFailure] = useState<string>();
  const busy = writes.prompt !== null || writes.running !== null;

  function remove(): void {
    setAsking(false);
    setFailure(undefined);
    void writes.attempt({
      key: DELETE_KEY,
      run: async () => {
        try {
          await api.deleteMode(mode.id);
        } catch (rejection) {
          const error = toApiError(rejection).error;
          if (isStepUpRequired(error)) {
            throw rejection;
          }
          if (!alive.current) {
            return;
          }
          if (error.api?.status === 404) {
            onGone();
          } else {
            setFailure(error.api?.status === 409 ? error.api.message : describeError(error));
          }
          return;
        }
        if (alive.current) {
          toast.success(deleted(mode.name));
          await navigate({ to: "/modes" });
        }
        forgetMode(queryClient, mode.id);
      },
    });
  }

  return (
    <div className="flex flex-col gap-2">
      <Button
        type="button"
        variant="destructive"
        className="self-start"
        disabled={busy}
        onClick={() => setAsking(true)}
      >
        {DELETE}
      </Button>
      {asking ? (
        <Alert>
          <AlertDescription className="flex flex-col gap-2 text-foreground">
            <p>{deleteQuestion(mode.name)}</p>
            <div className="flex gap-2">
              <Button type="button" variant="destructive" disabled={busy} onClick={remove}>
                {DELETE_MODE}
              </Button>
              <Button type="button" variant="outline" onClick={() => setAsking(false)}>
                {CANCEL}
              </Button>
            </div>
          </AlertDescription>
        </Alert>
      ) : null}
      {writes.prompt === DELETE_KEY ? (
        <StepUpPrompt
          id="step-up-delete"
          pending={writes.running !== null}
          onConfirm={writes.confirm}
          onCancel={writes.cancel}
        />
      ) : null}
      {writes.failure?.key === DELETE_KEY ? (
        <p role="alert" className="text-sm text-destructive">
          {writes.failure.message}
        </p>
      ) : null}
      {failure === undefined ? null : (
        <p role="alert" className="text-sm text-destructive">
          {failure}
        </p>
      )}
    </div>
  );
}
