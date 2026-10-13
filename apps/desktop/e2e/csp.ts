// The e2e build's CSP (Plan.md P8.9, ADR-0017): the release CSP of src-tauri/tauri.conf.json,
// with only `connect-src` changed to `'self'`, the origin fetchInvoke sends to instead of
// Tauri's IPC. So every spec runs under the CSP that ships: an injected `<style>` or an
// evaluated string is blocked and reported (fixtures.ts fails the spec).

/** A CSP's directives, by name, in their order. */
export function directives(csp: string): Map<string, string> {
  const parsed = new Map<string, string>();
  for (const part of csp.split(";")) {
    const [name = "", ...sources] = part.trim().split(/\s+/);
    if (name !== "") {
      parsed.set(name, sources.join(" "));
    }
  }
  return parsed;
}

/** `release` with `connect-src 'self'`. */
export function e2eCsp(release: string): string {
  const parsed = directives(release);
  parsed.set("connect-src", "'self'");
  return [...parsed].map(([name, sources]) => `${name} ${sources}`).join("; ");
}
