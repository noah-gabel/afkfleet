// A refused request's wait, shown (Plan.md P8.7, ADR-0017): the server's message and "Try
// again in N s." counting down, then "You can try again now." once (useRetryWait). Screen
// readers hear the start and the end, never each second: the alert's spoken text names the
// whole wait and never changes, the ticking number is hidden from them, and the end is a
// status that appears once.
import type { RetryWait } from "../hooks/useRetryWait";

/** Said once when a wait ends. */
export const TRY_AGAIN = "You can try again now.";

/** `wait`, counting down, or its end. */
export function RetryWaitMessage({ wait }: { wait: RetryWait }) {
  const { waiting } = wait;
  if (waiting !== null) {
    return (
      <p role="alert" className="text-sm text-destructive">
        {`${waiting.message} `}
        <span aria-hidden="true">{`Try again in ${waiting.secondsLeft} s.`}</span>
        <span className="sr-only">
          {`Try again in ${waiting.seconds} ${waiting.seconds === 1 ? "second" : "seconds"}.`}
        </span>
      </p>
    );
  }
  if (wait.ended) {
    return (
      <p role="status" className="text-sm">
        {TRY_AGAIN}
      </p>
    );
  }
  return null;
}
