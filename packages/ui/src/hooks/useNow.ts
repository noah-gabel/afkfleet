// The time now, for what counts on by itself (Plan.md P11.9; ADR-0020, R93): read again
// every `intervalMs` while mounted, never more often.
import { useEffect, useState } from "react";

/** Milliseconds since the epoch, read again every `intervalMs`. */
export function useNow(intervalMs: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    setNow(Date.now());
    const tick = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(tick);
  }, [intervalMs]);
  return now;
}
