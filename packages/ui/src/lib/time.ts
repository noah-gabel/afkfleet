// The API's times (UTC, `2027-01-15T08:00:00.000Z`), shown in the user's own time zone and
// locale; with the zone's name where the text may be read elsewhere (an invitation).
const FORMAT = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });
const WITH_ZONE = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "long" });

/** `iso` as the user reads a date and time. */
export function formatTime(iso: string): string {
  return FORMAT.format(new Date(iso));
}

/** `iso` with its time zone, for a text sent to someone else. */
export function formatTimeWithZone(iso: string): string {
  return WITH_ZONE.format(new Date(iso));
}
