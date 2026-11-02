// One lifecycle call on a bot (Plan.md P11.9, P11.11; ADR-0019, S50, S52; ADR-0020, U38, U48, U88, S73,
// S124, R102–R106), for its screen's controls and its card's button: one call at a time, each a
// direct call, as nothing about it belongs in a cache. An answer is never written over the cached bot,
// since a live frame may already be newer (R102): the bot is read again, its frames held meanwhile
// (S60). A call its agent was sent says so until the bot changes or the next action, and not again
// when the bot comes back to that state (R106). A refusal shows by the control (U48): a 409's or a
// 403's message, after which the bot is read again, as it is after a 404, which shows the screen's own
// 404. A 429 counts down on the controls that spend the bot's limit, never on Stop, which the limit
// exempts (S52, R104). A start or a stop that its bot's agent can't be told of yet says so, the same
// way (S125).
//
// A card reads the list, not the bot, so it says `onCard`. The list costs a read per loaded page, and
// a live frame patches it anyway, so a success reads it only while the watch is down (R115); a refusal
// says the cache is wrong, so it always does, which also takes away a button (403) or a card (404). A
// card may have no next action, so its refusal goes when the bot's state changes too.
import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { BOTS_LIST_KEY, botKey } from "../../../api/queries";
import { useAppStores } from "../../../app/context";
import type { BotCommandResponse } from "../../../generated/BotCommandResponse";
import type { BotResponse } from "../../../generated/BotResponse";
import type { CommandError } from "../../../generated/CommandError";
import { type RetryWait, useRetryWait } from "../../../hooks/useRetryWait";
import { useAlive } from "../../../hooks/useStepGuard";
import type { Control } from "../controls";

/** The refusals after which the bot is read again: its state or its sight changed. */
const READ_AGAIN: ReadonlySet<number> = new Set([403, 404, 409]);

/** What a bot's state and wish read as, to tell when the bot changed. */
function stateOf(bot: Pick<BotResponse, "status" | "desired_state">): string {
  return JSON.stringify([bot.desired_state, bot.status]);
}

/** The text of a refusal: the server's own message for a 409 or a 403. */
function refusal(error: CommandError): string {
  const status = error.api?.status;
  return error.api !== undefined && (status === 409 || status === 403)
    ? error.api.message
    : describeError(error);
}

/** What a call leaves to say: which note, and the bot's state it's about. */
interface Note {
  readonly kind: "sent" | "saved";
  readonly at: string;
}

/** One bot's lifecycle calls. */
export interface BotControl {
  /** The call that's running. */
  readonly running: Control | null;
  /** A call was sent to its agent, and the bot hasn't changed since. */
  readonly sent: boolean;
  /** A start or a stop was saved that its agent can't be told of yet, and the bot hasn't changed. */
  readonly saved: boolean;
  /** Why the last call was refused. */
  readonly failure: string | null;
  readonly wait: RetryWait;
  /** Whether `control` can't be used now. */
  locked(control: Control): boolean;
  /** Runs `control`, unless a call is running. */
  run(control: Control): Promise<void>;
}

/** `bot`'s lifecycle calls; `onCard` when they're offered on its card. */
export function useBotControl(bot: BotResponse, { onCard = false } = {}): BotControl {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const { live } = useAppStores();
  const alive = useAlive();
  const wait = useRetryWait();
  const [running, setRunning] = useState<Control | null>(null);
  const [note, setNote] = useState<Note | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const shown = stateOf(bot);
  // The bot changed since the call: its note goes for good (R106).
  useEffect(() => {
    setNote((held) => (held === null || held.at === shown ? held : null));
  }, [shown]);
  // A card's refusal goes with the state it was about. A 403's stays, as it says why the button went:
  // the viewer's level isn't part of the state.
  const [refusedAt, setRefusedAt] = useState(shown);
  useEffect(() => {
    if (onCard && refusedAt !== shown) {
      setFailure(null);
    }
  }, [onCard, refusedAt, shown]);

  /** The call behind `control`, and the note its answer leaves, if any. */
  async function call(control: Control): Promise<Note | null> {
    const sent = (answer: BotCommandResponse): Note | null =>
      answer.effect === "sent" ? { kind: "sent", at: stateOf(answer.bot) } : null;
    const saved = (answer: BotResponse): Note | null =>
      answer.status.kind === "unknown" ? { kind: "saved", at: stateOf(answer) } : null;
    switch (control) {
      case "start":
        return saved(await api.startBot(bot.id));
      case "stop":
        return saved(await api.stopBot(bot.id));
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

  async function run(control: Control): Promise<void> {
    if (running !== null) {
      return;
    }
    setRunning(control);
    setFailure(null);
    setNote(null);
    wait.reset();
    let readAgain = true;
    let refused = false;
    try {
      const left = await call(control);
      if (alive.current) {
        setNote(left);
      }
    } catch (rejection) {
      const error = toApiError(rejection).error;
      readAgain = READ_AGAIN.has(error.api?.status ?? 0);
      refused = readAgain;
      if (!wait.start(error) && alive.current) {
        setRefusedAt(shown);
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
    if (onCard && (refused || (readAgain && !live.getState().connected))) {
      void queryClient.invalidateQueries({ queryKey: BOTS_LIST_KEY, exact: true });
    }
  }

  return {
    running,
    sent: note?.kind === "sent" && note.at === shown,
    saved: note?.kind === "saved" && note.at === shown,
    failure,
    wait,
    locked: (control) => running !== null || (wait.waiting !== null && control !== "stop"),
    run,
  };
}
