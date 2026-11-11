import { describe, expect, it } from "vitest";

import type { CommandError } from "../generated/CommandError";
import { describeError, OLDER_SERVER } from "./describeError";

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

  it("says the server is older when it asks for a step-up the app no longer does (ADR-0028)", () => {
    const text = describeError(refusal("step_up_required"));

    expect(text).toBe(
      "This server is older than the app and still asks for your password here. Ask its owner to update it. (request 01900000-0000-7000-8000-000000000009)",
    );
    expect(text.startsWith(OLDER_SERVER)).toBe(true);
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

  it("shows the updater's own sentences as they came (ADR-0022, U33)", () => {
    expect(
      describeError({ code: "update_invalid", message: "The download couldn't be verified." }),
    ).toBe("The download couldn't be verified.");
    expect(
      describeError({ code: "no_update", message: "There's no update to install. Check again." }),
    ).toBe("There's no update to install. Check again.");
  });

  it("falls back to the fixed sentence for api without details", () => {
    expect(describeError({ code: "api", message: "The server refused the request." })).toBe(
      "The server refused the request.",
    );
  });
});
