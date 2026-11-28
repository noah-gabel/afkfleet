// What the guards report for a console error (e2e/errors.ts): a failed resource names its URL,
// so a failure such as the Windows runner's `net::ERR_NO_BUFFER_SPACE` says which request it was.
import { expect, test } from "@playwright/test";

import { consoleError } from "../errors";

test("a resource that failed to load names its URL", () => {
  expect(
    consoleError(
      "Failed to load resource: net::ERR_NO_BUFFER_SPACE",
      "http://127.0.0.1:4173/assets/index-abc123.css",
    ),
  ).toBe(
    "Failed to load resource: net::ERR_NO_BUFFER_SPACE (http://127.0.0.1:4173/assets/index-abc123.css)",
  );
});

test("any other error stays its text, and a failure without a URL too", () => {
  expect(consoleError("Uncaught TypeError: x is undefined", "http://127.0.0.1:4173/a.js")).toBe(
    "Uncaught TypeError: x is undefined",
  );
  expect(consoleError("Failed to load resource: net::ERR_FAILED", "")).toBe(
    "Failed to load resource: net::ERR_FAILED",
  );
});
