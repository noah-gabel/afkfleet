// Phase 8's DoD as one story (Plan.md P8.9): the Owner invites someone (the step-up prompt,
// the code shown once and copied), logs out; the invitee registers with the copied code,
// sets up 2FA and logs in; the Owner logs in again and finds them in the user list.

import { OWNER, PASSWORD } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, logIn, setUp2fa } from "./steps";

test("an invite goes from the Owner to a new user who sets up 2FA", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expect(page.getByText(`Signed in as ${OWNER}`, { exact: false })).toBeVisible();

  await page.getByRole("link", { name: "Invites" }).click();
  await page.getByRole("button", { name: "Create invite" }).click();
  await page.getByLabel("Current password").fill(PASSWORD);
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByText(/shown only this once/)).toBeVisible();
  const code = backend.newestInviteCode() ?? "";
  await expect(page.getByText(code, { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Copy code" }).click();
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  await page.getByRole("button", { name: "Done" }).click();
  await expect(page.getByText(code, { exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "Log out" }).click();

  await page.getByRole("link", { name: "Register with an invite" }).click();
  await page.getByLabel("Invite code").fill(copied);
  await page.getByLabel("Username").fill("afkbot9");
  await page.getByLabel("Password", { exact: true }).fill(PASSWORD);
  await page.getByLabel("Repeat password", { exact: true }).fill(PASSWORD);
  await page.getByRole("button", { name: "Register" }).click();
  await setUp2fa(page, backend, "afkbot9");
  await page.getByRole("button", { name: "Log out" }).click();

  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await page.getByRole("link", { name: "Users" }).click();
  const row = page.getByRole("row").filter({ hasText: "afkbot9" });

  expect(copied).toBe(code);
  await expect(row.getByText("Member")).toBeVisible();
  await expect(row.getByText("Set up")).toBeVisible();
});
