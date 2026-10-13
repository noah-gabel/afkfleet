// fetchInvoke without a browser (Plan.md P8.9): what each answer becomes, alone and through
// the real TauriApiClient.
import { ApiError } from "@afkfleet/ui";
import { expect, test } from "@playwright/test";

import { TauriApiClient } from "../../src/TauriApiClient";
import { fetchInvoke, IPC_PATH } from "../app/fetchInvoke";

const REFUSAL = { code: "no_session", message: "You're not logged in." };

/** A `fetch` that answers `body` as text, recording what was sent. */
function answering(body: string, sent: { url?: string; init?: RequestInit } = {}): typeof fetch {
  return async (url, init) => {
    sent.url = String(url);
    sent.init = init ?? {};
    return new Response(body, { status: 200 });
  };
}

test("posts the args as JSON to the command's path", async () => {
  const sent: { url?: string; init?: RequestInit } = {};
  const invoke = fetchInvoke(answering(JSON.stringify({ ok: true, value: 7 }), sent));

  const value = await invoke<number>("users", { before: "x" });

  expect(value).toBe(7);
  expect(sent.url).toBe(`${IPC_PATH}users`);
  expect(sent.init?.method).toBe("POST");
  expect(sent.init?.body).toBe(JSON.stringify({ before: "x" }));
});

test("resolves an answer without a value with undefined", async () => {
  const invoke = fetchInvoke(answering(JSON.stringify({ ok: true })));

  expect(await invoke("logout")).toBeUndefined();
});

test("rejects with the CommandError a refusal carries", async () => {
  const invoke = fetchInvoke(answering(JSON.stringify({ ok: false, error: REFUSAL })));

  await expect(invoke("me")).rejects.toEqual(REFUSAL);
});

test.describe("anything else rejects with a plain string", () => {
  for (const [name, send] of [
    ["a body that isn't JSON", answering("<html></html>")],
    ["JSON that isn't an envelope", answering(JSON.stringify({ value: 1 }))],
    [
      "a failed fetch",
      (async () => {
        throw new TypeError("network");
      }) as typeof fetch,
    ],
  ] as const) {
    test(name, async () => {
      const rejection = await fetchInvoke(send)("me").catch((error: unknown) => error);

      expect(typeof rejection).toBe("string");
    });
  }
});

test("through TauriApiClient: a refusal is an ApiError of it, anything else `ipc`", async () => {
  const refused = new TauriApiClient(
    fetchInvoke(answering(JSON.stringify({ ok: false, error: REFUSAL }))),
  );
  const broken = new TauriApiClient(fetchInvoke(answering("not json")));

  const first = await refused.me().catch((error: unknown) => error);
  const second = await broken.me().catch((error: unknown) => error);

  expect(first).toBeInstanceOf(ApiError);
  expect((first as ApiError).error).toEqual(REFUSAL);
  expect((second as ApiError).error.code).toBe("ipc");
});
