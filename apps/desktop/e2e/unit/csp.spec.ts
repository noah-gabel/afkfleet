// The e2e build's CSP (Plan.md P8.9): the release CSP of tauri.conf.json with only
// `connect-src` changed, and what `vite preview` really serves.
import { readFileSync } from "node:fs";

import { expect, test } from "@playwright/test";

import { directives, e2eCsp } from "../csp";

/** The release CSP, as tauri.conf.json holds it. */
const RELEASE: string = JSON.parse(
  readFileSync(new URL("../../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
).app.security.csp;

test("is the release CSP with only connect-src 'self'", () => {
  const release = directives(RELEASE);
  const e2e = directives(e2eCsp(RELEASE));

  expect(e2e.get("connect-src")).toBe("'self'");
  release.delete("connect-src");
  e2e.delete("connect-src");
  expect(e2e).toEqual(release);
  expect(release.size).toBeGreaterThan(0);
});

test("is what the preview serves with the page", async ({ request }) => {
  const page = await request.get("/");

  expect(page.headers()["content-security-policy"]).toBe(e2eCsp(RELEASE));
});
