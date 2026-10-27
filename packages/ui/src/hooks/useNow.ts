// The time now, for what counts on by itself (Plan.md P11.9; ADR-0020, R93, R100): each
// render reads the clock, and the next render comes when what's shown changes next, as
// `nextIn` says, never later than a minute; so a count is never late by up to a tick.
import { useEffect, useState } from "react";

/** The longest wait between two renders. */
const MAX_WAIT_MS = 60_000;

/**
 * Milliseconds since the epoch, read at each render; renders again `nextIn(now)`
 * milliseconds later, at most a minute.
 */
export function useNow(nextIn: (now: number) => number): number {
  const [, setTicks] = useState(0);
  const now = Date.now();
  const wait = Math.min(MAX_WAIT_MS, Math.max(1, nextIn(now)));
  // Every render schedules the next one from its own reading, whatever caused it.
  useEffect(() => {
    const tick = setTimeout(() => setTicks((ticks) => ticks + 1), wait);
    return () => clearTimeout(tick);
  });
  return now;
}
