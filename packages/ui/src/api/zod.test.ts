// The release CSP has no 'unsafe-eval', so zod's JIT probe (`new Function("")`, caught,
// but reported as a violation at every start) must never run. Each test file runs in its
// own worker, so nothing in this file has parsed before the spy is set.
import { afterEach, describe, expect, it, vi } from "vitest";

describe("the UI's zod", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("parses the API's inputs and the forms without constructing a Function", async () => {
    const constructed = vi.spyOn(globalThis, "Function");
    const { validated } = await import("./validated");
    const { FakeApiClient } = await import("./FakeApiClient");
    const { isCommandError } = await import("./ApiError");
    const { registerForm } = await import("../features/auth/schemas");
    const { serverForm } = await import("../features/server/schemas");

    const step = await validated(
      new FakeApiClient({ login: async () => ({ step: "mfa_required", expires_in: 300 }) }),
    ).login({ username: "afkbot1", password: "placeholder password" });
    const registered = registerForm.safeParse({
      invite: " afk_iv_x ",
      username: "afkbot1",
      password: "placeholder password",
      repeat: "placeholder password",
    });
    const server = serverForm(false).safeParse({
      url: "https://example.com",
      useCa: false,
      caPem: "",
    });
    const known = isCommandError({ code: "ipc", message: "x" });

    expect(step.step).toBe("mfa_required");
    expect(registered.success).toBe(true);
    expect(server.success).toBe(true);
    expect(known).toBe(true);
    expect(constructed).not.toHaveBeenCalled();
  });
});
