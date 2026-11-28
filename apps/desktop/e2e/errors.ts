// What the guards report for a console error (e2e/fixtures.ts).

/** The start of the browser's own message for a resource it couldn't load. */
const FAILED_RESOURCE = "Failed to load resource";

/**
 * A console error as the guards report it: its text, and for a resource the browser failed to
 * load, the resource's URL, which its text leaves out (the message's location holds it). The
 * Windows runner's `net::ERR_NO_BUFFER_SPACE` came twice without saying which request it was.
 */
export function consoleError(text: string, url: string): string {
  return text.startsWith(FAILED_RESOURCE) && url !== "" ? `${text} (${url})` : text;
}
