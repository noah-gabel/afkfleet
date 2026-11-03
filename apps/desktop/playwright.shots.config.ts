// The screenshot baselines (Plan.md P11.11; ADR-0020, U97, U125, S130, S131, R226): the theme
// sweep's rows, the shots' own proofs and the fonts' spec, in Linux Chromium inside Playwright's
// pinned image, which `just shots` builds and runs (scripts/shots.mjs). Each sweep row measures in
// Edge (playwright.config.ts) and shoots here. It refuses to load anywhere else, so no other
// machine's pixels become a baseline. With AFKFLEET_SHOTS_DESIGN set, every shot is written to
// target/design instead, for a before or an after.
import process from "node:process";

import { defineConfig, devices } from "@playwright/test";

import { BASE_URL } from "./e2e/base";
import { COMPARE, DESIGN_MARKER, refusal, SHOTS_PROJECT, WIDE } from "./e2e/shots";

const refused = refusal(process.platform, process.env);
if (refused !== undefined) {
  throw new Error(refused);
}

const ci = process.env.CI !== undefined;
const design = process.env[DESIGN_MARKER] === "1";

export default defineConfig({
  testDir: "e2e/specs",
  testMatch: ["theme-sweep.spec.ts", "shots.spec.ts", "fonts.spec.ts"],
  outputDir: "../../target/shots/results",
  snapshotPathTemplate: design
    ? "../../target/design/{testFileName}/{arg}{ext}"
    : "{testDir}/../baselines/{testFileName}/{arg}{ext}",
  updateSnapshots: "none",
  fullyParallel: true,
  forbidOnly: ci,
  retries: 0,
  // The orphans' check fails a compare that left a baseline no row shot.
  reporter: ci
    ? [["list"], ["github"], ["./e2e/orphans-reporter.ts"]]
    : [["list"], ["./e2e/orphans-reporter.ts"]],
  expect: { toHaveScreenshot: COMPARE },
  projects: [
    {
      name: SHOTS_PROJECT,
      use: {
        ...devices["Desktop Chrome"],
        channel: "chromium",
        baseURL: BASE_URL,
        viewport: WIDE,
        deviceScaleFactor: 1,
        timezoneId: "UTC",
        locale: "en-US",
        permissions: ["clipboard-read", "clipboard-write"],
        // The same pixels on every run: software rasterization in one pass, in sRGB.
        launchOptions: {
          args: [
            "--disable-gpu",
            "--disable-gpu-rasterization",
            "--disable-partial-raster",
            "--force-color-profile=srgb",
            "--disable-lcd-text",
          ],
        },
      },
    },
  ],
  webServer: {
    // Not `pnpm exec`, which would check the image's install again and can't write it.
    command: "node_modules/.bin/vite preview --config vite.e2e.config.ts",
    url: BASE_URL,
    reuseExistingServer: false,
    timeout: 60_000,
  },
});
