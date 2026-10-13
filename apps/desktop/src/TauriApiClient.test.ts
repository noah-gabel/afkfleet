import { API_METHODS, ApiError, type ApiMethod } from "@afkfleet/ui";
import { describe, expect, it } from "vitest";

import contract from "../src-tauri/tests/ipc-contract.json";
import { type Invoke, TauriApiClient } from "./TauriApiClient";

/** One call `invoke` received. */
interface Invoked {
  command: string;
  args: unknown;
}

/** A client over a fake `invoke` that records each call and answers `answer`. */
function client(answer: () => Promise<unknown> = async () => undefined) {
  const invoked: Invoked[] = [];
  const invoke: Invoke = async <T>(command: string, args?: unknown) => {
    invoked.push({ command, args });
    return (await answer()) as T;
  };
  return { api: new TauriApiClient(invoke), invoked };
}

/** Calls `method` on `api` with `inputs`, as the contract names them. */
function call(api: TauriApiClient, method: string, inputs: readonly unknown[]): Promise<unknown> {
  expect(API_METHODS).toContain(method);
  const bound = api[method as ApiMethod] as (...args: unknown[]) => Promise<unknown>;
  return bound.apply(api, [...inputs]);
}

describe("TauriApiClient", () => {
  for (const entry of contract.commands) {
    it(`${entry.method} invokes ${entry.command} with exactly the contract's args`, async () => {
      const { api, invoked } = client();

      await call(api, entry.method, entry.inputs);

      expect(invoked).toEqual([{ command: entry.command, args: entry.args }]);
    });
  }

  it("has one contract entry per method, in the same order", () => {
    expect(contract.commands.map((entry) => entry.method)).toEqual([...API_METHODS]);
  });

  it("rejects with an ApiError of the CommandError the command answered", async () => {
    const answered = { code: "no_session", message: "You're not logged in." };
    const { api } = client(async () => {
      throw answered;
    });

    const rejection = await api.me().catch((error: unknown) => error);

    expect(rejection).toBeInstanceOf(ApiError);
    expect((rejection as ApiError).error).toEqual(answered);
  });

  it("rejects with ipc for Tauri's own refusal, without its text", async () => {
    const { api } = client(async () => {
      throw "Command me not allowed by ACL";
    });

    const rejection = await api.me().catch((error: unknown) => error);

    expect(rejection).toBeInstanceOf(ApiError);
    expect((rejection as ApiError).error.code).toBe("ipc");
    expect(JSON.stringify((rejection as ApiError).error)).not.toContain("ACL");
  });

  it("answers what the command answered", async () => {
    const { api } = client(async () => ({ state: "none" }));

    expect(await api.serverProfile()).toEqual({ state: "none" });
  });
});
