// A mode's Delete (Plan.md P11.9; ADR-0020, U66, S16, S17, R162, R169): shown from the server's
// flag, it asks first, then goes straight through `useStepUpAction`; afterwards, back to the list.
// A refusal shows in place, and the mode stays. `mode_in_use` also lists the bots the viewer can
// see that run the mode, from every page of their bots, read then and kept nowhere else. Only that
// whole read may say the bot is one they can't see, and only the latest attempt's answer shows.
import { useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { useRef, useState } from "react";
import { toast } from "sonner";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { readEveryBot } from "../../../api/queries";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import type { BotResponse } from "../../../generated/BotResponse";
import type { ModeResponse } from "../../../generated/ModeResponse";
import { useAlive } from "../../../hooks/useStepGuard";
import { isStepUpRequired } from "../../../lib/stepUp";
import { StepUpPrompt } from "../../admin/components/StepUpPrompt";
import type { useStepUpAction } from "../../admin/hooks/useStepUpAction";
import { forgetMode } from "../cache";
import {
  A_BOT_YOU_CANT_SEE,
  BOTS_THAT_USE_IT,
  botsFailed,
  CANCEL,
  DELETE,
  DELETE_MODE,
  deleted,
  deleteQuestion,
} from "../texts";

/** The write's key: one at a time per screen. */
const DELETE_KEY = "delete";

/** What's known of the bots that run a mode a delete was refused for. */
type UsedBy =
  | { readonly state: "read"; readonly bots: readonly BotResponse[] }
  | { readonly state: "failed"; readonly why: string };

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
  const [usedBy, setUsedBy] = useState<UsedBy | null>(null);
  // The attempt whose bots may show: an older read's answer is dropped.
  const attempt = useRef(0);
  const busy = writes.prompt !== null || writes.running !== null;

  /** Reads which of the viewer's bots run the mode, for the refusal on the screen. */
  async function readUsers(): Promise<void> {
    const mine = attempt.current;
    const current = () => alive.current && attempt.current === mine;
    try {
      const bots = (await readEveryBot(api)).filter((bot) => bot.mode_id === mode.id);
      if (current()) {
        setUsedBy({ state: "read", bots });
      }
    } catch (rejection) {
      if (current()) {
        setUsedBy({ state: "failed", why: describeError(toApiError(rejection).error) });
      }
    }
  }

  function remove(): void {
    setAsking(false);
    setFailure(undefined);
    setUsedBy(null);
    attempt.current += 1;
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
            if (error.api?.code === "mode_in_use") {
              void readUsers();
            }
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
      {usedBy === null ? null : <UsedByList usedBy={usedBy} />}
    </div>
  );
}

/** The bots at a refused delete: each linking to its screen, or that none can be seen. */
function UsedByList({ usedBy }: { usedBy: UsedBy }) {
  if (usedBy.state === "failed") {
    return <p className="text-sm text-destructive">{botsFailed(usedBy.why)}</p>;
  }
  if (usedBy.bots.length === 0) {
    return <p className="text-sm">{A_BOT_YOU_CANT_SEE}</p>;
  }
  return (
    <div className="flex flex-col gap-1 text-sm">
      <p>{BOTS_THAT_USE_IT}</p>
      <ul className="flex flex-col gap-1">
        {usedBy.bots.map((bot) => (
          <li key={bot.id}>
            <Link to="/bots/$id" params={{ id: bot.id }} className="underline">
              {bot.account.name}
            </Link>
          </li>
        ))}
      </ul>
    </div>
  );
}
