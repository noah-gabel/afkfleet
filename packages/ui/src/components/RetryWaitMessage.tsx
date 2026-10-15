// A refused request's wait, shown (Plan.md P8.7, ADR-0017): the server's message and "Try
// again in N s." counting down (minutes, or hours and minutes, for a long wait; the user's
// change in Phase 9, group F), then "You can try again now." once (useRetryWait). Screen
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
        <span aria-hidden="true">{visibleWait(waiting.secondsLeft)}</span>
        <span className="sr-only">{spokenWait(waiting.seconds)}</span>
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

/**
 * A wait of `seconds` in the largest unit that keeps it readable: seconds up to a minute,
 * whole minutes up to an hour, then hours and minutes. Minutes round up, so the text never
 * says a wait is over before the server would take the request.
 */
function parts(seconds: number): { hours: number; minutes: number } | null {
  if (seconds <= 60) {
    return null;
  }
  const minutes = Math.ceil(seconds / 60);
  return minutes <= 60
    ? { hours: 0, minutes }
    : { hours: Math.floor(minutes / 60), minutes: minutes % 60 };
}

/** What the screen shows of a wait of `seconds` (see `parts`). */
export function visibleWait(seconds: number): string {
  const wait = parts(seconds);
  if (wait === null) {
    return `Try again in ${seconds} s.`;
  }
  if (wait.hours === 0) {
    return `Try again in ${wait.minutes} min.`;
  }
  return wait.minutes === 0
    ? `Try again in ${wait.hours} h.`
    : `Try again in ${wait.hours} h ${wait.minutes} min.`;
}

/** `count` and its unit in words: "1 minute", "2 minutes". */
function counted(count: number, unit: string): string {
  return `${count} ${unit}${count === 1 ? "" : "s"}`;
}

/** What a screen reader hears of a wait of `seconds`, once (see `parts`). */
export function spokenWait(seconds: number): string {
  const wait = parts(seconds);
  if (wait === null) {
    return `Try again in ${counted(seconds, "second")}.`;
  }
  if (wait.hours === 0) {
    return `Try again in ${counted(wait.minutes, "minute")}.`;
  }
  return wait.minutes === 0
    ? `Try again in ${counted(wait.hours, "hour")}.`
    : `Try again in ${counted(wait.hours, "hour")} ${counted(wait.minutes, "minute")}.`;
}
