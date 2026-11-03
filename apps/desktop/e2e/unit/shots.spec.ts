// The screenshot baselines' settings (Plan.md P11.11; ADR-0020, U97, S130, S131): they're made only
// in the pinned image, compared to the last pixel, and named after their row.
import { expect, test } from "@playwright/test";

import { COMPARE, IMAGE_MARKER, refusal, slug } from "../shots";

test("the shots run only in the pinned Linux image", () => {
  expect(refusal("linux", { [IMAGE_MARKER]: "1" })).toBeUndefined();
  for (const [platform, env] of [
    ["win32", { [IMAGE_MARKER]: "1" }],
    ["darwin", { [IMAGE_MARKER]: "1" }],
    ["linux", {}],
    ["linux", { [IMAGE_MARKER]: "yes" }],
  ] as const) {
    expect(refusal(platform, env), `${platform} ${JSON.stringify(env)}`).toMatch(/just shots/);
  }
});

test("a shot is compared to the last pixel, never with a tolerance", () => {
  expect(COMPARE).toEqual({ threshold: 0, animations: "disabled", caret: "hide", scale: "css" });
  expect(COMPARE).not.toHaveProperty("maxDiffPixels");
  expect(COMPARE).not.toHaveProperty("maxDiffPixelRatio");
});

test("a row's name becomes its file's name", () => {
  expect(slug("Home, every status")).toBe("home-every-status");
  expect(slug("a bot's screen, stacked")).toBe("a-bot-s-screen-stacked");
  expect(slug("2FA setup")).toBe("2fa-setup");
});
