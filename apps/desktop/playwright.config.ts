// Playwright for the e2e build (Plan.md P8.9, ADR-0017): the installed Microsoft Edge, the
// engine of the app's WebView2, so nothing is downloaded (`channel: "msedge"`; no
// `playwright install`). `vite preview` serves dist-e2e/, which `just e2e` builds first.
// The specs are e2e/**/*.spec.ts: the browser specs in e2e/specs, Node-only ones in e2e/unit.
import { defineConfig, devices } from "@playwright/test";

import { BASE_URL } from "./e2e/base";

const ci = process.env.CI !== undefined;

export default defineConfig({
  testDir: "e2e",
  testMatch: "**/*.spec.ts",
  outputDir: "test-results",
  forbidOnly: ci,
  retries: 0,
  reporter: ci ? [["list"], ["github"]] : "list",
  use: {
    ...devices["Desktop Edge"],
    channel: "msedge",
    baseURL: BASE_URL,
    permissions: ["clipboard-read", "clipboard-write"],
  },
  webServer: {
    command: "pnpm exec vite preview --config vite.e2e.config.ts",
    url: BASE_URL,
    reuseExistingServer: false,
    timeout: 60_000,
  },
});
