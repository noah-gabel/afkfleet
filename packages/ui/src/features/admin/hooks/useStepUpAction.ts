// Running an admin write that needs a step-up (Plan.md P8.8, ADR-0016's amendment,
// ADR-0017). Role changes, disabling and enabling, and creating and revoking invites need
// the password within the last 5 minutes, but the server doesn't tell the app when a
// step-up ends. So the write is sent; on 403 `step_up_required` its row or form asks for the
// password ("Confirm it's you"), steps up with a direct call, and sends the same write once
// more. Several writes within the 5 minutes ask once; a retry the server refuses again shows
// its message and never asks twice. The server alone decides.
//
// One write at a time per screen. A write's `run` carries no secret: the step-up's password
// goes only through `confirm`, from StepUpForm, which empties its field.
import { useRef, useState } from "react";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import type { CommandError } from "../../../generated/CommandError";
import { useAlive } from "../../../hooks/useStepGuard";
import { isStepUpRequired } from "../../../lib/stepUp";

/** One admin write, under the key of its row or form. */
export interface AdminWrite {
  readonly key: string;
  run(): Promise<void>;
}

/** A write's failure, shown at its row or form. */
export interface WriteFailure {
  readonly key: string;
  readonly message: string;
}

/**
 * Admin writes with their step-up; see the module comment. `describe` words a failure
 * (describeError's text by default); `onRefused` runs after one (a refetch).
 */
export function useStepUpAction(
  options: { describe?: (error: CommandError) => string; onRefused?: () => void } = {},
) {
  const api = useApiClient();
  const alive = useAlive();
  const busy = useRef(false);
  const [running, setRunning] = useState<string | null>(null);
  const [prompt, setPrompt] = useState<AdminWrite | null>(null);
  const [failure, setFailure] = useState<WriteFailure | null>(null);

  function fail(key: string, error: CommandError): void {
    if (alive.current) {
      setFailure({ key, message: (options.describe ?? describeError)(error) });
    }
    options.onRefused?.();
  }

  /** Runs `write.run`, once at a time; `retry` is the one after a step-up. */
  async function send(write: AdminWrite, retry: boolean): Promise<void> {
    busy.current = true;
    setRunning(write.key);
    try {
      await write.run();
    } catch (rejection) {
      const error = toApiError(rejection).error;
      if (!retry && isStepUpRequired(error)) {
        if (alive.current) {
          setPrompt(write);
        }
      } else {
        fail(write.key, error);
      }
    } finally {
      busy.current = false;
      if (alive.current) {
        setRunning(null);
      }
    }
  }

  /** Sends `write`, asking for the password if the server wants a step-up. */
  async function attempt(write: AdminWrite): Promise<void> {
    if (busy.current || prompt !== null) {
      return;
    }
    setFailure(null);
    await send(write, false);
  }

  /** Steps up with `password`, then sends the waiting write once more; throws for the form. */
  async function confirm(password: string): Promise<void> {
    const write = prompt;
    if (write === null || busy.current) {
      return;
    }
    await api.stepUp({ password });
    if (!alive.current) {
      return;
    }
    setPrompt(null);
    await send(write, true);
  }

  /** Drops the waiting write. */
  function cancel(): void {
    setPrompt(null);
  }

  return {
    attempt,
    confirm,
    cancel,
    /** The key of the write that waits for the password. */
    prompt: prompt?.key ?? null,
    /** The key of the write in flight. */
    running,
    failure,
  };
}
