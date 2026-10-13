// The app's webview freezes `Object.prototype` (tauri.conf.json's `freezePrototype`,
// ADR-0017). A library that assigns a shadowing property such as `toString` on a plain
// object then throws, so the input checks run here with it frozen, as in the app. Vitest
// runs each test file in its own worker, so the freeze stays in this file.
import { describe, expect, it } from "vitest";

Object.freeze(Object.prototype);

describe("with a frozen Object.prototype", () => {
  it("still validates inputs and refuses broken ones", async () => {
    const { FakeApiClient } = await import("./FakeApiClient");
    const { validated } = await import("./validated");
    const { toApiError } = await import("./ApiError");
    const api = validated(
      new FakeApiClient({ login: async () => ({ step: "mfa_required", expires_in: 300 }) }),
    );

    const step = await api.login({ username: "AfkBot1", password: "placeholder password" });
    const refused = await api.loginMfa("").catch((error: unknown) => error);

    expect(Object.isFrozen(Object.prototype)).toBe(true);
    expect(step).toEqual({ step: "mfa_required", expires_in: 300 });
    expect(toApiError(refused).error.code).toBe("invalid_input");
  });
});
