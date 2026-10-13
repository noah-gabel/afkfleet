// What the e2e specs do on a page, as a user does it (Plan.md P8.9).
import type { Page } from "@playwright/test";
import { type FakeBackend, PASSWORD } from "../fake/backend";
import { expect } from "../fixtures";

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
  await expect(page.getByText(`Signed in as ${username}`, { exact: false })).toBeVisible();
  return copied;
}
