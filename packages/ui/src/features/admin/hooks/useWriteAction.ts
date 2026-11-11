// Running a write from a row or a form (Plan.md P8.8, ADR-0017, ADR-0028): one at a time per
// screen, under the key of the row or form it belongs to, where its failure shows. No write here
// asks for the password again: once logged in, a session is enough, and only the profile's two
// flows (a new password, a new authenticator) still confirm it's you (ADR-0028, which supersedes
// ADR-0016's step-up for every other write). The server alone decides.
//
// A write's `run` carries no secret.
import { useRef, useState } from "react";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import type { CommandError } from "../../../generated/CommandError";
import { useAlive } from "../../../hooks/useStepGuard";

/** One write, under the key of its row or form. */
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
 * Writes, one at a time; see the module comment. `describe` words a failure (describeError's
 * text by default); `onRefused` runs after one (a refetch).
 */
export function useWriteAction(
  options: { describe?: (error: CommandError) => string; onRefused?: () => void } = {},
) {
  const alive = useAlive();
  const busy = useRef(false);
  const [running, setRunning] = useState<string | null>(null);
  const [failure, setFailure] = useState<WriteFailure | null>(null);

  /** Sends `write`, unless one is in flight; its failure shows under its key. */
  async function attempt(write: AdminWrite): Promise<void> {
    if (busy.current) {
      return;
    }
    setFailure(null);
    busy.current = true;
    setRunning(write.key);
    try {
      await write.run();
    } catch (rejection) {
      const error = toApiError(rejection).error;
      if (alive.current) {
        setFailure({ key: write.key, message: (options.describe ?? describeError)(error) });
      }
      options.onRefused?.();
    } finally {
      busy.current = false;
      if (alive.current) {
        setRunning(null);
      }
    }
  }

  /** Drops write `key`'s failure, as its form closes; another write's stays. */
  function forget(key: string): void {
    setFailure((shown) => (shown?.key === key ? null : shown));
  }

  return {
    attempt,
    forget,
    /** The key of the write in flight. */
    running,
    failure,
  };
}
