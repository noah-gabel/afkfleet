import { API_METHODS, ApiError, type ApiMethod } from "@afkfleet/ui";
import { describe, expect, it } from "vitest";

import contract from "../src-tauri/tests/ipc-contract.json";
import { type Invoke, type OpenChannel, TauriApiClient } from "./TauriApiClient";

/** One call `invoke` received. */
interface Invoked {
  command: string;
  args: unknown;
}

/** What the fake `openChannel` answers: the value Tauri reads a channel from. */
const CHANNEL = "__CHANNEL__:0";

/**
 * A client over a fake `invoke` that records each call and answers `answer`, and a fake
 * `openChannel` that keeps each channel's `onMessage`.
 */
function client(answer: () => Promise<unknown> = async () => undefined) {
  const invoked: Invoked[] = [];
  const channels: ((message: unknown) => void)[] = [];
  const invoke: Invoke = async <T>(command: string, args?: unknown) => {
    invoked.push({ command, args });
    return (await answer()) as T;
  };
  const openChannel: OpenChannel = (onMessage) => {
    channels.push(onMessage as (message: unknown) => void);
    return CHANNEL;
  };
  return { api: new TauriApiClient(invoke, openChannel), invoked, channels };
}

/** The contract's inputs as values: `<onUpdate>` is a callback. */
function values(inputs: readonly unknown[]): unknown[] {
  return inputs.map((input) => (input === "<onUpdate>" ? () => undefined : input));
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

      await call(api, entry.method, values(entry.inputs));

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

describe("TauriApiClient's watch", () => {
  it("hands each update on until the call settles, and none after", async () => {
    let settle: () => void = () => undefined;
    const { api, channels } = client(
      () =>
        new Promise<undefined>((resolve) => {
          settle = () => resolve(undefined);
        }),
    );
    const got: unknown[] = [];

    const watching = api.watchEvents((update) => got.push(update));
    const send = channels[0];
    if (send === undefined) {
      throw new Error("no channel was opened");
    }
    send({ update: "refetch", reason: "ready" });
    settle();
    await watching;
    send({ update: "refetch", reason: "resync" });

    expect(got).toEqual([{ update: "refetch", reason: "ready" }]);
  });

  it("rejects as the command did, and hands nothing on after", async () => {
    const { api, channels } = client(async () => {
      throw { code: "no_session", message: "You're not logged in." };
    });
    const got: unknown[] = [];

    const rejection = await api.watchEvents((update) => got.push(update)).catch((e: unknown) => e);
    channels[0]?.({ update: "refetch", reason: "ready" });

    expect((rejection as ApiError).error.code).toBe("no_session");
    expect(got).toEqual([]);
  });
});
