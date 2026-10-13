import { describe, expect, it } from "vitest";

import type { CommandError } from "../generated/CommandError";
import { describeError } from "./describeError";

/** A refusal from the server with `code`, cast as the generated type allows only known ones. */
function refusal(code: string, extra: Record<string, unknown> = {}): CommandError {
  return {
    code: "api",
    message: "The server refused the request.",
    api: {
      status: 400,
      code,
      message: "Something only a newer server says.",
      request_id: "01900000-0000-7000-8000-000000000009",
      ...extra,
    },
  } as CommandError;
}

describe("describeError", () => {
  it("shows a server code this app doesn't know with its message and request ID", () => {
    const text = describeError(refusal("from_a_newer_server"));

    expect(text).toBe(
      "Something only a newer server says. (request 01900000-0000-7000-8000-000000000009)",
    );
  });

  it("adds the wait to a rate limit or a busy server", () => {
    expect(describeError(refusal("rate_limited", { retry_after_secs: 12 }))).toBe(
      "Something only a newer server says. Try again in 12 s.",
    );
    expect(describeError(refusal("busy"))).toBe("Something only a newer server says.");
  });

  it("shows the app's own fixed sentence for every other code", () => {
    expect(describeError({ code: "unreachable", message: "The server can't be reached." })).toBe(
      "The server can't be reached.",
    );
    expect(describeError({ code: "ipc", message: "The app couldn't run the request." })).toBe(
      "The app couldn't run the request.",
    );
  });

  it("falls back to the fixed sentence for api without details", () => {
    expect(describeError({ code: "api", message: "The server refused the request." })).toBe(
      "The server refused the request.",
    );
  });
});
