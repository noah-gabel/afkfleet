// The API's times (UTC, `2027-01-15T08:00:00.000Z`), shown in the user's own time zone and
// locale.
const FORMAT = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });

/** `iso` as the user reads a date and time. */
export function formatTime(iso: string): string {
  return FORMAT.format(new Date(iso));
}
