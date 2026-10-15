// An account screen's writes (Plan.md P9.8, ADR-0018 group E): reviewing, deleting and every
// grant write need the password within the last 5 minutes, so they go through the admin
// screens' `useStepUpAction`. One write at a time per screen; each write names how its
// refusals read, since one code (409 `conflict`) means something else for each. After a
// refusal, every account query is read again.
import { useQueryClient } from "@tanstack/react-query";
import { useRef } from "react";

import { describeError } from "../../../api/describeError";
import { ACCOUNTS_KEY } from "../../../api/queries";
import type { CommandError } from "../../../generated/CommandError";
import { useStepUpAction } from "../../admin/hooks/useStepUpAction";

/** How a write's refusal reads. */
export type Describe = (error: CommandError) => string;

/** The screen's writes; see the module comment. */
export function useAccountWrites() {
  const queryClient = useQueryClient();
  const describing = useRef<Describe>(describeError);
  const writes = useStepUpAction({
    describe: (error) => describing.current(error),
    onRefused: () => void queryClient.invalidateQueries({ queryKey: ACCOUNTS_KEY }),
  });

  /** Sends `run` under `key`, its refusals worded by `describe`. */
  function attempt(key: string, run: () => Promise<void>, describe: Describe = describeError) {
    if (writes.running !== null || writes.prompt !== null) {
      return;
    }
    describing.current = describe;
    void writes.attempt({ key, run });
  }

  return {
    attempt,
    confirm: writes.confirm,
    cancel: writes.cancel,
    prompt: writes.prompt,
    running: writes.running,
    failure: writes.failure,
    /** Whether a write runs or waits for the password. */
    busy: writes.running !== null || writes.prompt !== null,
  };
}

/** A screen's writes, as its parts take them. */
export type AccountWrites = ReturnType<typeof useAccountWrites>;
