// How the app reads a duration (Plan.md P11.9; ADR-0020, R93): like the retry waits, in
// whole units, the two largest that matter: "45 s", "12 min", "3 h 12 min", "2 d 4 h".
// A duration below zero (a clock ahead of the PC's) reads as none.

const MINUTE_S = 60;
const HOUR_S = 60 * MINUTE_S;
const DAY_S = 24 * HOUR_S;

/** `ms` as the app reads it. */
export function formatDuration(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  if (seconds < MINUTE_S) {
    return `${seconds} s`;
  }
  if (seconds < HOUR_S) {
    return `${Math.floor(seconds / MINUTE_S)} min`;
  }
  if (seconds < DAY_S) {
    const hours = Math.floor(seconds / HOUR_S);
    const minutes = Math.floor((seconds % HOUR_S) / MINUTE_S);
    return minutes === 0 ? `${hours} h` : `${hours} h ${minutes} min`;
  }
  const days = Math.floor(seconds / DAY_S);
  const hours = Math.floor((seconds % DAY_S) / HOUR_S);
  return hours === 0 ? `${days} d` : `${days} d ${hours} h`;
}
