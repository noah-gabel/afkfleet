import { describe, expect, it } from "vitest";

import { API_METHODS } from "./ApiClient";
import { ApiError } from "./ApiError";
import { FakeApiClient, fakeApiError, fakeWatch } from "./FakeApiClient";

describe("FakeApiClient", () => {
  it("answers through the scripted handler and records the call", async () => {
    const fake = new FakeApiClient({
      login: async () => ({ step: "mfa_required", expires_in: 300 }),
    });

    const step = await fake.login({ username: "AfkBot1", password: "placeholder password" });

    expect(step).toEqual({ step: "mfa_required", expires_in: 300 });
    expect(fake.calls).toEqual([
      { method: "login", args: [{ username: "AfkBot1", password: "placeholder password" }] },
    ]);
  });

  it("records every argument, undefined ones included", async () => {
    const page = { items: [] };
    const fake = new FakeApiClient({
      sessions: async () => page,
      patchUser: async () => undefined as never,
    });

    await fake.sessions(undefined, 10);
    await fake.patchUser("01900000-0000-7000-8000-000000000001", { disabled: true });

    expect(fake.calls).toEqual([
      { method: "sessions", args: [undefined, 10] },
      { method: "patchUser", args: ["01900000-0000-7000-8000-000000000001", { disabled: true }] },
    ]);
  });

  it("rejects every unscripted method with an ApiError naming it", async () => {
    const fake = new FakeApiClient();

    for (const method of API_METHODS) {
      const call = (fake[method] as () => Promise<unknown>).call(fake);

      await expect(call).rejects.toBeInstanceOf(ApiError);
      await expect(call).rejects.toThrow(method);
    }
    expect(fake.calls.map((call) => call.method)).toEqual([...API_METHODS]);
  });

  it("passes a handler's rejection through", async () => {
    const fake = new FakeApiClient({
      me: async () => {
        throw fakeApiError("no_session");
      },
    });

    await expect(fake.me()).rejects.toMatchObject({ error: { code: "no_session" } });
  });
});

describe("fakeApiError", () => {
  it("builds an error with the code, and the details given", () => {
    const api = {
      status: 401,
      code: "invalid_credentials" as const,
      message: "The username, password or code is wrong.",
      request_id: "r1",
    };

    expect(fakeApiError("unreachable").error).toEqual({
      code: "unreachable",
      message: "fake unreachable",
    });
    expect(fakeApiError("api", { api }).error).toEqual({
      code: "api",
      message: "fake api",
      api,
    });
    expect(
      fakeApiError("invalid_input", { fields: [{ path: "code", message: "x" }] }).error.fields,
    ).toEqual([{ path: "code", message: "x" }]);
  });
});

describe("fakeWatch", () => {
  it("holds each watch until the test ends it, handing on what the test pushes", async () => {
    const watch = fakeWatch();
    const fake = new FakeApiClient({ watchEvents: watch.handler });
    const got: unknown[] = [];

    const watching = fake.watchEvents((update) => got.push(update));
    watch.latest().push({ update: "refetch", reason: "ready" });
    watch.latest().end();
    watch.latest().push({ update: "refetch", reason: "resync" });
    await watching;

    expect(got).toEqual([{ update: "refetch", reason: "ready" }]);
    expect(watch.latest().ended).toBe(true);
  });

  it("stops the older watch when a newer one starts, as the app does", async () => {
    const watch = fakeWatch();
    const fake = new FakeApiClient({ watchEvents: watch.handler });

    const older = fake.watchEvents(() => undefined);
    const newer = fake.watchEvents(() => undefined);

    await expect(older).resolves.toBeUndefined();
    expect(watch.calls.map((call) => call.ended)).toEqual([true, false]);
    watch.latest().fail(fakeApiError("no_session"));
    await expect(newer).rejects.toMatchObject({ error: { code: "no_session" } });
  });

  it("names no watch before one started", () => {
    expect(() => fakeWatch().latest()).toThrow("no watch started");
  });
});
