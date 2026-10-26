// MSW's handlers for the e2e build (Plan.md P8.9, ADR-0017): every command fetchInvoke sends
// to /e2e/ipc/<command> goes to the test's FakeBackend, and its outcome comes back as a 200
// with fetchInvoke's envelope. They run in the Playwright process through @msw/playwright,
// over Playwright's routing: nothing of MSW reaches the page.
import { HttpResponse, http } from "msw";

import { IPC_PATH } from "../app/fetchInvoke";
import { BASE_URL } from "../base";
import type { FakeBackend } from "./backend";

/** The handlers that answer from `backend`. */
export function handlers(backend: FakeBackend) {
  return [
    http.post(`${BASE_URL}${IPC_PATH}:command`, async ({ request, params }) => {
      const args = (await request.json()) as Record<string, unknown>;
      const command = String(params.command);
      try {
        return HttpResponse.json(await backend.handle(command, args));
      } catch {
        return HttpResponse.json({ ok: false, error: `the fake doesn't know ${command}` });
      }
    }),
  ];
}
