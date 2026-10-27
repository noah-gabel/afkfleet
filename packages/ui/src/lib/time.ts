// The API's times (UTC, `2027-01-15T08:00:00.000Z`), shown in the user's own time zone and
// locale; with the zone's name where the text may be read elsewhere (an invitation).
const FORMAT = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });
const WITH_ZONE = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "long" });
const CLOCK = new Intl.DateTimeFormat(undefined, { timeStyle: "short" });
const DATE = new Intl.DateTimeFormat(undefined, { dateStyle: "medium" });
const CLOCK_SECONDS = new Intl.DateTimeFormat(undefined, { timeStyle: "medium" });

/** `iso` as the user reads a date and time. */
export function formatTime(iso: string): string {
  return FORMAT.format(new Date(iso));
}

/** `iso` with its time zone, for a text sent to someone else. */
export function formatTimeWithZone(iso: string): string {
  return WITH_ZONE.format(new Date(iso));
}

/** `iso` as a time of day, for something that ends within the hour (a link's code). */
export function formatClock(iso: string): string {
  return CLOCK.format(new Date(iso));
}

/** `iso`'s day, as the user reads a date: the app's date format, without the time. */
export function formatDate(iso: string): string {
  return DATE.format(new Date(iso));
}

/** `iso` as a time of day to the second, the timeline's (U46). */
export function formatClockSeconds(iso: string): string {
  return CLOCK_SECONDS.format(new Date(iso));
}

/** Whether `iso` falls on the day `now` (ms since the epoch) does, in the user's time zone. */
export function sameDay(iso: string, now: number): boolean {
  const at = new Date(iso);
  const today = new Date(now);
  return (
    at.getFullYear() === today.getFullYear() &&
    at.getMonth() === today.getMonth() &&
    at.getDate() === today.getDate()
  );
}
