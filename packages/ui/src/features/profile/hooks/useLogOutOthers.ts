// "Log out all other sessions" (Plan.md P8.7, ADR-0017): the remedy for a lost or stolen
// PC, whose session can't be told apart in the list. It covers every session the server
// lists, not only the pages on screen (an older session, like a stolen PC's, may sit on a
// later page), so it fetches every page first, then ends each other session in turn; one
// already gone counts as ended. None of it carries a secret.
//
// It runs to the end even when the user leaves the screen: stopping halfway could leave a
// stolen PC's session alive with nobody seeing the report. The report shows on the screen
// if it's still there, otherwise as a toast.
import { useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { toast } from "sonner";

import type { ApiClient } from "../../../api/ApiClient";
import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { SESSIONS_KEY } from "../../../api/queries";
import type { CommandError } from "../../../generated/CommandError";
import { useAlive } from "../../../hooks/useStepGuard";

/** What ending the other sessions did. */
export interface OthersReport {
  readonly text: string;
  /** Whether anything failed. */
  readonly failed: boolean;
}

/** Ends every other session the server lists; see the module comment. */
export async function logOutOthers(api: ApiClient): Promise<OthersReport> {
  const others: string[] = [];
  let listError: CommandError | undefined;
  let before: string | undefined;
  try {
    do {
      const page = await api.sessions(before);
      others.push(...page.items.filter((session) => !session.current).map((s) => s.id));
      before = page.next;
    } while (before !== undefined);
  } catch (rejection) {
    listError = toApiError(rejection).error;
  }
  let ended = 0;
  const failures: CommandError[] = [];
  for (const id of others) {
    try {
      await api.revokeSession(id);
      ended += 1;
    } catch (rejection) {
      const error = toApiError(rejection).error;
      if (error.api?.status === 404) {
        ended += 1;
      } else {
        failures.push(error);
      }
    }
  }
  return report(ended, failures, listError);
}

/** The report's text. */
function report(
  ended: number,
  failures: readonly CommandError[],
  listError: CommandError | undefined,
): OthersReport {
  const parts: string[] = [];
  if (ended > 0 || (failures.length === 0 && listError === undefined)) {
    parts.push(
      ended === 0
        ? "There were no other sessions."
        : `Logged out ${ended} other ${ended === 1 ? "session" : "sessions"}.`,
    );
  }
  const [first] = failures;
  if (first !== undefined) {
    parts.push(`${failures.length} couldn't be logged out: ${describeError(first)} Try again.`);
  }
  if (listError !== undefined) {
    parts.push(
      `The list of your sessions couldn't be read to the end: ${describeError(listError)} Try again.`,
    );
  }
  return { text: parts.join(" "), failed: first !== undefined || listError !== undefined };
}

/** Running `logOutOthers` from a screen; see the module comment. */
export function useLogOutOthers() {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const alive = useAlive();
  const busy = useRef(false);
  const [running, setRunning] = useState(false);
  const [shown, setShown] = useState<OthersReport>();

  async function run(): Promise<void> {
    if (busy.current) {
      return;
    }
    busy.current = true;
    setRunning(true);
    setShown(undefined);
    const done = await logOutOthers(api);
    busy.current = false;
    await queryClient.invalidateQueries({ queryKey: SESSIONS_KEY });
    if (alive.current) {
      setRunning(false);
      setShown(done);
    } else if (done.failed) {
      toast.warning(done.text);
    } else {
      toast.success(done.text);
    }
  }

  return { run, running, report: shown };
}
