// The banner's retries (Plan.md P8.6, ADR-0017): while the server is down, one
// `checkServer()` (health_ready) every 10 s; while it's busy, after the wait it asked for
// (at least 10 s). Each wait starts only once the previous check has ended, so two never
// run at once, and "Retry now" does nothing while one runs. A refused certificate isn't
// retried on a timer, only on request.
//
// A successful check needs nothing here: the watched client takes the banner down for any
// call that started after it went up, and refetches what failed (app/watched.ts). A busy
// or rate-limited answer switches to waiting; any failure other than the banner's own
// states keeps it up with its text.
import { useCallback, useEffect, useRef, useState } from "react";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { useAppStores, useConnection } from "../../../app/context";

/** The shortest wait between two checks, in seconds. */
export const CHECK_EVERY_SECS = 10;

/** The banner's state and its "Retry now". */
export function useServerRetry() {
  const api = useApiClient();
  const { connection } = useAppStores();
  const status = useConnection((state) => state.status);
  const retryAfterSecs = useConnection((state) => state.retryAfterSecs);
  const [checking, setChecking] = useState(false);
  const running = useRef(false);

  const check = useCallback(async () => {
    if (running.current) {
      return;
    }
    running.current = true;
    setChecking(true);
    try {
      await api.checkServer();
    } catch (rejection) {
      const error = toApiError(rejection).error;
      const refusal = error.api?.code;
      if (error.code === "api" && (refusal === "busy" || refusal === "rate_limited")) {
        connection
          .getState()
          .busy(Math.max(CHECK_EVERY_SECS, error.api?.retry_after_secs ?? CHECK_EVERY_SECS));
      } else if (
        error.code !== "unreachable" &&
        error.code !== "timed_out" &&
        error.code !== "certificate"
      ) {
        connection.getState().checkFailed(describeError(error));
      }
    } finally {
      running.current = false;
      setChecking(false);
    }
  }, [api, connection]);

  // The wait, the check, then the next wait, for as long as the banner is down or busy. A
  // change of state (the check's own answer, or another call's) starts over with the new
  // wait; a check still running is never doubled (`running`).
  useEffect(() => {
    if (status !== "down" && status !== "busy") {
      return;
    }
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const wait = () => {
      const state = connection.getState();
      const seconds =
        state.status === "busy" ? (state.retryAfterSecs ?? CHECK_EVERY_SECS) : CHECK_EVERY_SECS;
      timer = setTimeout(() => {
        void check().then(() => {
          if (!stopped) {
            wait();
          }
        });
      }, seconds * 1000);
    };
    wait();
    return () => {
      stopped = true;
      clearTimeout(timer);
    };
  }, [status, check, connection]);

  return {
    status,
    retryAfterSecs,
    checking,
    retryNow: () => {
      void check();
    },
  };
}
