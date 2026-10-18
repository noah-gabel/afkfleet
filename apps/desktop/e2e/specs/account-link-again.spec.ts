// Linking an account again (Plan.md P9.8): an account Microsoft no longer accepts shows the
// re-auth prompt on its card and its screen; "Link again" leads to the link screen for that
// account, which ends with it linked again and active.

import { LINK_CODE, MEMBER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, FLOW_END_MS, logIn } from "./steps";

test("an account that needs a new sign-in is linked again", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  backend.addAccount(MEMBER, "reauth_required");
  await page.goto("/");
  await logIn(page, MEMBER);
  await enterCode(page, backend.code(MEMBER));

  await page.getByRole("link", { name: "Accounts" }).click();
  await expect(page.getByText("Microsoft no longer accepts this account's sign-in.")).toBeVisible();
  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  await expect(
    page.getByText(
      "Microsoft no longer accepts this account's sign-in. Link it again to keep using it.",
    ),
  ).toBeVisible();
  await page.getByRole("link", { name: "Link again" }).click();
  await expect(page.getByRole("heading", { name: "Link AfkBot1 again" })).toBeVisible();
  await page.getByRole("button", { name: "Get a code" }).click();
  await expect(page.getByText(LINK_CODE, { exact: true })).toBeVisible();

  await expect(page.getByText("AfkBot1 is linked again.")).toBeVisible({ timeout: FLOW_END_MS });
  await page.getByRole("link", { name: "Open account" }).click();
  await expect(page.getByText("Active", { exact: true })).toBeVisible();
  await expect.poll(() => backend.opened()).toBe(1);
});
