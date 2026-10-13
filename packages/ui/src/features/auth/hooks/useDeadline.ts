// A sign-in step's deadline (Plan.md P8.7, ADR-0017): the server's `expires_in` gives it,
// and an expired step answers like a wrong code, so the app ends it itself. One timer
// raises the last-minute warning, one ends the step.
import { useEffect, useRef, useState } from "react";

/** How long before the deadline the warning shows, in ms. */
export const WARN_BEFORE_MS = 60_000;

/** Whether the step is in its last minute; calls `onExpire` at `deadline` (ms since epoch). */
export function useDeadline(deadline: number, onExpire: () => void): boolean {
  const [lastMinute, setLastMinute] = useState(() => deadline - Date.now() <= WARN_BEFORE_MS);
  const expire = useRef(onExpire);
  useEffect(() => {
    expire.current = onExpire;
  }, [onExpire]);
  useEffect(() => {
    const now = Date.now();
    const warn = setTimeout(
      () => setLastMinute(true),
      Math.max(0, deadline - WARN_BEFORE_MS - now),
    );
    const end = setTimeout(() => expire.current(), Math.max(0, deadline - now));
    return () => {
      clearTimeout(warn);
      clearTimeout(end);
    };
  }, [deadline]);
  return lastMinute;
}

/** "This step ends in 5 minutes.", from the step's `expires_in` in seconds. */
export function endsIn(expiresIn: number): string {
  const minutes = Math.round(expiresIn / 60);
  if (minutes < 1) {
    return "This step ends in under a minute.";
  }
  return `This step ends in ${minutes} ${minutes === 1 ? "minute" : "minutes"}.`;
}
