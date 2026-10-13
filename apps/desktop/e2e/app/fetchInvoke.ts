// The e2e build's `invoke` (Plan.md P8.9, ADR-0017): each command is a POST of its args as
// JSON to /e2e/ipc/<command> on the page's own origin, which MSW's handlers answer in the
// Playwright process (e2e/fake/). It settles like Tauri's: an answer resolves with its value,
// a refusal rejects with the `CommandError` it carries, and anything else rejects with a
// plain string, which TauriApiClient turns into `ipc`, as it does Tauri's own refusals.
//
// Every answer is a 200 with an envelope, `{ ok: true, value }` or `{ ok: false, error }`,
// so the browser logs no failed request: every spec fails on a console error.
//
// It lives only in the e2e entry. The shipped app never imports it (Biome refuses the path in
// apps/desktop/src), and scripts/e2e-bundle-guard.mjs checks both shipped bundles.
import type { Invoke } from "../../src/TauriApiClient";

/** Where every command is sent. */
export const IPC_PATH = "/e2e/ipc/";

/** What the e2e server answers. */
type Envelope =
  | { readonly ok: true; readonly value?: unknown }
  | { readonly ok: false; readonly error: unknown };

/** An answer's envelope, or nothing when the body isn't one. */
function envelope(body: unknown): Envelope | undefined {
  if (typeof body !== "object" || body === null || !("ok" in body)) {
    return undefined;
  }
  if (body.ok === true) {
    return { ok: true, value: "value" in body ? body.value : undefined };
  }
  if (body.ok === false && "error" in body) {
    return { ok: false, error: body.error };
  }
  return undefined;
}

/** An `invoke` over `fetch`; see the module comment. */
export function fetchInvoke(send: typeof fetch = fetch): Invoke {
  return async <T>(command: string, args?: Parameters<Invoke>[1]): Promise<T> => {
    let text: string;
    try {
      const response = await send(`${IPC_PATH}${command}`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(args ?? {}),
      });
      text = await response.text();
    } catch {
      return Promise.reject("the e2e server can't be reached");
    }
    let body: unknown;
    try {
      body = JSON.parse(text);
    } catch {
      return Promise.reject("the e2e server's answer isn't JSON");
    }
    const answer = envelope(body);
    if (answer === undefined) {
      return Promise.reject("the e2e server's answer isn't an envelope");
    }
    if (answer.ok) {
      return answer.value as T;
    }
    return Promise.reject(answer.error);
  };
}
