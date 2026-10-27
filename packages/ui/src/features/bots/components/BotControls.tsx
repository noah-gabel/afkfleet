// A bot's lifecycle controls (Plan.md P11.9; ADR-0019, S50, S52; ADR-0020, U38, U48, S67,
// S73, R102–R106): the controls its state offers (controls.ts) that the viewer's level allows
// (permissions.ts); a viewer whose level allows none reads one muted line instead.
//
// One call at a time, each a direct call: nothing about it belongs in a cache. An answer is
// never written over the cached bot, since a live frame may already be newer (R102): the bot
// is read again, its frames held meanwhile (S60). A call its agent was sent says so until the
// bot changes or the next action, and not again when the bot comes back to that state (R106).
// A refusal shows by the controls (U48): a 409's or a 403's message, after which the bot is
// read again, as it is after a 404, which shows the screen's own 404.
// A 429 counts down on the controls that spend the bot's limit, never on Stop, which the
// limit exempts (S52, R104).
import { useQueryClient } from "@tanstack/react-query";
import { LoaderCircleIcon } from "lucide-react";
import { useEffect, useState } from "react";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { botKey } from "../../../api/queries";
import { RetryWaitMessage } from "../../../components/RetryWaitMessage";
import { Button } from "../../../components/ui/button";
import type { BotCommandResponse } from "../../../generated/BotCommandResponse";
import type { BotResponse } from "../../../generated/BotResponse";
import type { CommandError } from "../../../generated/CommandError";
import { useRetryWait } from "../../../hooks/useRetryWait";
import { useAlive } from "../../../hooks/useStepGuard";
import { type Control, controlsFor } from "../controls";
import { botActions } from "../permissions";
import { CONTROL, SENT_NOTE, VIEW_ONLY } from "../texts";

/** The refusals after which the bot is read again: its state or its sight changed. */
const READ_AGAIN: ReadonlySet<number> = new Set([403, 404, 409]);

/** What a bot's state and wish read as, to tell when the bot changed. */
function stateOf(bot: Pick<BotResponse, "status" | "desired_state">): string {
  return JSON.stringify([bot.desired_state, bot.status]);
}

/** `bot`'s controls. */
export function BotControls({ bot }: { bot: BotResponse }) {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const alive = useAlive();
  const wait = useRetryWait();
  const [running, setRunning] = useState<Control | null>(null);
  /** The bot's state when a call was sent to its agent. */
  const [sentAt, setSentAt] = useState<string | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const shown = stateOf(bot);
  // The bot changed since the call was sent: its note goes for good (R106).
  useEffect(() => {
    setSentAt((sent) => (sent === null || sent === shown ? sent : null));
  }, [shown]);

  if (!botActions(bot.account.level).control) {
    return <p className="text-sm text-muted-foreground">{VIEW_ONLY}</p>;
  }

  /** The call behind `control`: its answer's state when its agent was sent it, else null. */
  async function call(control: Control): Promise<string | null> {
    const sent = (answer: BotCommandResponse) =>
      answer.effect === "sent" ? stateOf(answer.bot) : null;
    switch (control) {
      case "start":
        await api.startBot(bot.id);
        return null;
      case "stop":
        await api.stopBot(bot.id);
        return null;
      case "restart":
        return sent(await api.restartBot(bot.id));
      case "resume":
        return sent(await api.resumeBot(bot.id));
      case "reset":
        return sent(await api.resetBot(bot.id));
      default: {
        const unknown: never = control;
        return unknown;
      }
    }
  }

  /** The text of a refusal: the server's own message for a 409 or a 403. */
  function refusal(error: CommandError): string {
    const status = error.api?.status;
    return error.api !== undefined && (status === 409 || status === 403)
      ? error.api.message
      : describeError(error);
  }

  async function run(control: Control): Promise<void> {
    if (running !== null) {
      return;
    }
    setRunning(control);
    setFailure(null);
    setSentAt(null);
    wait.reset();
    let readAgain = true;
    try {
      const sent = await call(control);
      if (alive.current) {
        setSentAt(sent);
      }
    } catch (rejection) {
      const error = toApiError(rejection).error;
      readAgain = READ_AGAIN.has(error.api?.status ?? 0);
      if (!wait.start(error) && alive.current) {
        setFailure(refusal(error));
      }
    } finally {
      if (alive.current) {
        setRunning(null);
      }
    }
    if (readAgain) {
      void queryClient.invalidateQueries({ queryKey: botKey(bot.id), exact: true });
    }
  }

  const waiting = wait.waiting !== null;
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap gap-2">
        {controlsFor(bot).map((control) => (
          <Button
            key={control}
            type="button"
            variant="outline"
            size="sm"
            disabled={running !== null || (waiting && control !== "stop")}
            onClick={() => {
              void run(control);
            }}
          >
            {running === control ? (
              <LoaderCircleIcon aria-hidden="true" className="animate-spin" />
            ) : null}
            {CONTROL[control]}
          </Button>
        ))}
      </div>
      {sentAt !== null && sentAt === shown ? (
        <p role="status" className="text-sm text-muted-foreground">
          {SENT_NOTE}
        </p>
      ) : null}
      {failure === null ? null : (
        <p role="alert" className="text-sm text-destructive">
          {failure}
        </p>
      )}
      <RetryWaitMessage wait={wait} />
    </div>
  );
}
