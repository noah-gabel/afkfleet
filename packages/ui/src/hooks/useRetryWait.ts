// A refused request's wait (Plan.md P8.7, ADR-0017). A 429 (a rate limit, or a locked
// username) and a 503 (a busy server) carry the server's `Retry-After`. The form that got one
// shows it counting down (RetryWaitMessage), keeps its submit disabled until it reaches zero,
// then says once that the user can try again; a click during the wait would only be refused
// again. Every form that sends a request uses it: login, the code step, the 2FA setup,
// registering, the step-up (the profile's and the admin prompt's), the new password and the
// server form.
//
// The countdown lives in the form: after a reload it's gone, and the next try shows the
// server's remaining wait again. The server enforces it either way.
import { useCallback, useEffect, useState } from "react";

import type { CommandError } from "../generated/CommandError";

/** A wait to start: the server's message and its `Retry-After`. */
export interface WaitRequest {
  readonly message: string;
  readonly seconds: number;
}

/** A wait that's running. */
export interface Waiting {
  /** The server's message, without the wait. */
  readonly message: string;
  /** The whole wait, in seconds. */
  readonly seconds: number;
  /** What's left of it, in whole seconds, rounded up. */
  readonly secondsLeft: number;
}

/** A form's wait after a refusal. */
export interface RetryWait {
  readonly waiting: Waiting | null;
  /** The wait just ended: the form says once that the user can try again. */
  readonly ended: boolean;
  /** Starts the wait `error` asks for, if it asks for one: true if it did. */
  start(error: CommandError): boolean;
  /** Starts `request`'s wait. */
  startFor(request: WaitRequest): void;
  /** Forgets an ended wait's notice, as a new submit does. */
  reset(): void;
}

/** The wait `error` asks for, if any: a refusal with the server's `Retry-After`. */
export function retryRequest(error: CommandError): WaitRequest | undefined {
  const seconds = error.api?.retry_after_secs;
  if (error.api === undefined || seconds === undefined || seconds <= 0) {
    return undefined;
  }
  return { message: error.api.message, seconds };
}

/** A running wait: its request and when it ends, in ms since the epoch. */
interface Running extends WaitRequest {
  readonly deadline: number;
}

/** A form's wait; see the module comment. */
export function useRetryWait(): RetryWait {
  const [running, setRunning] = useState<Running | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [ended, setEnded] = useState(false);

  // One tick a second from the start: the server's wait is whole seconds, so every tick
  // lands on a change of the shown number, and the one that reaches the deadline ends it.
  useEffect(() => {
    if (running === null) {
      return;
    }
    const tick = setInterval(() => {
      const at = Date.now();
      if (at >= running.deadline) {
        clearInterval(tick);
        setRunning(null);
        setEnded(true);
      } else {
        setNow(at);
      }
    }, 1000);
    return () => clearInterval(tick);
  }, [running]);

  const startFor = useCallback((request: WaitRequest) => {
    const at = Date.now();
    setRunning({ ...request, deadline: at + request.seconds * 1000 });
    setNow(at);
    setEnded(false);
  }, []);

  const start = useCallback(
    (error: CommandError) => {
      const request = retryRequest(error);
      if (request === undefined) {
        return false;
      }
      startFor(request);
      return true;
    },
    [startFor],
  );

  const reset = useCallback(() => setEnded(false), []);

  const waiting =
    running === null
      ? null
      : {
          message: running.message,
          seconds: running.seconds,
          secondsLeft: Math.max(1, Math.ceil((running.deadline - now) / 1000)),
        };
  return { waiting, ended, start, startFor, reset };
}
