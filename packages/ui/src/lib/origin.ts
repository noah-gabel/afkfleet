// How the UI writes a server's origin: the Rust side's `ServerUrl` ends in "/"
// (`https://example.com/`), which reads oddly in a sentence.

/** `url` without its trailing slash. */
export function displayOrigin(url: string): string {
  return url.endsWith("/") ? url.slice(0, -1) : url;
}
