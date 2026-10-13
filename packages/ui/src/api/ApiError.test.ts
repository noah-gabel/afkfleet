import { describe, expect, it } from "vitest";

import {
  ApiError,
  INVALID_INPUT_MESSAGE,
  IPC_MESSAGE,
  invalidInput,
  isCommandError,
  toApiError,
} from "./ApiError";

const refusal = {
  code: "api",
  message: "The server refused the request.",
  api: {
    status: 429,
    code: "rate_limited",
    message: "Too many requests. Try again later.",
    request_id: "01900000-0000-7000-8000-000000000009",
    retry_after_secs: 12,
  },
};

describe("isCommandError", () => {
  it("accepts what the Rust side answers", () => {
    expect(isCommandError({ code: "no_server", message: "No server is set up yet." })).toBe(true);
    expect(isCommandError(refusal)).toBe(true);
    expect(
      isCommandError({
        code: "invalid_input",
        message: INVALID_INPUT_MESSAGE,
        fields: [{ path: "url", message: "length must be at most 2048 bytes" }],
      }),
    ).toBe(true);
  });

  it("accepts a server code only a newer server knows", () => {
    const newer = { ...refusal, api: { ...refusal.api, code: "from_a_newer_server" } };

    expect(isCommandError(newer)).toBe(true);
  });

  it("refuses an app code this UI doesn't know, and anything else", () => {
    for (const value of [
      { code: "teleported", message: "x" },
      { code: "no_server" },
      "Command me not allowed by ACL",
      null,
      undefined,
      42,
      { code: "api", message: "x", api: { status: "429" } },
    ]) {
      expect(isCommandError(value)).toBe(false);
    }
  });
});

describe("toApiError", () => {
  it("keeps a CommandError as it came", () => {
    const error = toApiError(refusal);

    expect(error).toBeInstanceOf(ApiError);
    expect(error.error).toEqual(refusal);
    expect(error.message).toBe(refusal.message);
    expect(error.name).toBe("ApiError");
  });

  it("makes anything else ipc and drops its text", () => {
    for (const rejection of ["Command login not allowed by ACL", new Error("boom"), undefined]) {
      const error = toApiError(rejection);

      expect(error.error).toEqual({ code: "ipc", message: IPC_MESSAGE });
      expect(JSON.stringify(error.error)).not.toMatch(/ACL|boom/);
    }
  });

  it("passes an ApiError through", () => {
    const original = invalidInput([{ path: "code", message: "too long" }]);

    expect(toApiError(original)).toBe(original);
  });
});

describe("invalidInput", () => {
  it("names each broken field under invalid_input", () => {
    expect(invalidInput([{ path: "limit", message: "too big" }]).error).toEqual({
      code: "invalid_input",
      message: INVALID_INPUT_MESSAGE,
      fields: [{ path: "limit", message: "too big" }],
    });
  });
});
