// Linking a Microsoft account as one story (Plan.md P9.8): a Member gets a code (Microsoft's
// page opens by itself), the link waits for approval; the Owner sees how many wait on Home
// and in the nav, reviews it with "Confirm it's you"; the Member shares the account by
// username and deletes it.

import { LINK_CODE, MEMBER, NEW_USER, OWNER, PASSWORD } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, FLOW_END_MS, logIn } from "./steps";

test("a Member's link waits for the Owner's approval, then is shared and deleted", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  await page.goto("/");
  await logIn(page, MEMBER);
  await enterCode(page, backend.code(MEMBER));

  await page.getByRole("link", { name: "Accounts" }).click();
  await page.getByRole("link", { name: "Link Microsoft account" }).click();
  await expect(page.getByText(/^Only enter a code you started yourself here\./)).toBeVisible();
  expect(backend.opened()).toBe(0);
  await page.getByRole("button", { name: "Get a code" }).click();
  await expect(page.getByText(LINK_CODE, { exact: true })).toBeVisible();
  // The page opens after the code shows, through its own request: poll for it.
  await expect.poll(() => backend.opened()).toBe(1);
  await expect(
    page.getByText("AfkBot1 is linked. It waits for an Admin's or the Owner's approval", {
      exact: false,
    }),
  ).toBeVisible({ timeout: FLOW_END_MS });
  await expect(page.getByText(LINK_CODE, { exact: true })).toHaveCount(0);
  await page.getByRole("link", { name: "Back to accounts" }).click();
  await expect(page.getByText("Microsoft · Waiting for approval · Manage")).toBeVisible();
  await page.getByRole("button", { name: "Log out" }).click();

  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expect(page.getByText("1 Microsoft link is waiting for your approval.")).toBeVisible();
  await expect(
    page.getByRole("link", { name: "Accounts, 1 waiting for your approval" }),
  ).toBeVisible();
  await page.getByRole("link", { name: "Review" }).click();
  await page.getByRole("button", { name: "Approve AfkBot1" }).click();
  await page.getByRole("button", { name: "Approve", exact: true }).click();
  await page.getByLabel("Current password").fill(PASSWORD);
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByText("AfkBot1 approved.")).toBeVisible();
  await expect(page.getByRole("link", { name: "Accounts", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Log out" }).click();

  await logIn(page, MEMBER);
  await enterCode(page, backend.code(MEMBER));
  await page.getByRole("link", { name: "Accounts" }).click();
  await expect(page.getByText("Microsoft · Active · Manage")).toBeVisible();
  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  await page.getByLabel("Username").fill(NEW_USER);
  await page.getByRole("button", { name: "Share" }).click();
  await page.getByLabel("Current password").fill(PASSWORD);
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByText(`AfkBot1 is shared with ${NEW_USER}.`)).toBeVisible();
  await expect(page.getByLabel(`Access of ${NEW_USER}`)).toHaveValue("view");

  await page.getByRole("button", { name: "Delete", exact: true }).click();
  await page.getByRole("button", { name: "Delete account" }).click();
  await expect(page.getByText("AfkBot1 deleted.")).toBeVisible();
  await expect(
    page.getByText("No accounts yet. Link a Microsoft account to add one."),
  ).toBeVisible();
});
