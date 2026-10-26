// What the e2e specs do on a page, as a user does it (Plan.md P8.9).
import type { Page } from "@playwright/test";
import { type FakeBackend, PASSWORD } from "../fake/backend";
import { expect } from "../fixtures";

/**
 * How long a wait for a link flow's end may take. The fake flow reads `waiting` first and
 * `linked` from its second read on, and the link screen reads every 2 s
 * (`LINK_FLOW_POLL_MS`), so the end shows a poll interval and two round trips through MSW
 * after the code: on a loaded windows-latest runner that passed Playwright's default 5 s.
 * Every other wait keeps the default.
 */
export const FLOW_END_MS = 15_000;

/** The password step of the login form. */
export async function logIn(page: Page, username: string, password = PASSWORD): Promise<void> {
  await page.getByLabel("Username").fill(username);
  await page.getByLabel("Password", { exact: true }).fill(password);
  await page.getByRole("button", { name: "Log in" }).click();
}

/** A code at the code step. */
export async function enterCode(page: Page, code: string): Promise<void> {
  await page.getByLabel("Code").fill(code);
  await page.getByRole("button", { name: "Verify" }).click();
}

/**
 * The first login's 2FA setup, from its first screen to Home: the QR code and the key, the
 * confirming code, the recovery codes (copied, then checked off), and the next code.
 * Answers the recovery codes the clipboard held.
 */
export async function setUp2fa(page: Page, backend: FakeBackend, username: string) {
  await expect(
    page.getByRole("heading", { name: "Set up two-factor authentication" }),
  ).toBeVisible();
  await expect(page.getByRole("img", { name: "QR code for your authenticator app" })).toBeVisible();
  await expect(page.getByText("JBSW Y3DP EHPK 3PXP")).toBeVisible();
  await page.getByLabel("Code").fill(backend.setupCode());
  await page.getByRole("button", { name: "Confirm" }).click();
  await expect(page.getByRole("list", { name: "Recovery codes" })).toBeVisible();
  await page.getByRole("button", { name: "Copy all" }).click();
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  await page.getByLabel("I've saved these codes").check();
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByText(/Wait until your authenticator app shows a new code/)).toBeVisible();
  await enterCode(page, backend.code(username));
  await expectHome(page, username);
  return copied;
}

/** Home, for `username`: the bots' heading, and the Header's user, with `role` when given. */
export async function expectHome(page: Page, username: string, role?: string): Promise<void> {
  await expect(page.getByRole("heading", { name: "Bots" })).toBeVisible();
  const banner = page.getByRole("banner");
  await expect(banner.getByText(username, { exact: true })).toBeVisible();
  if (role !== undefined) {
    await expect(banner.getByText(role, { exact: true })).toBeVisible();
  }
}
