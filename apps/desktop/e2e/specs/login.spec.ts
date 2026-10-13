// Logging in with the 2FA code step (Plan.md P8.9), in Edge, under the release CSP.

import { OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, logIn } from "./steps";

test("logs in with the password, then a code, after one wrong code", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();

  await logIn(page, OWNER);
  await enterCode(page, "000000");
  await expect(page.getByText("Wrong code. 2 tries left.")).toBeVisible();
  await enterCode(page, backend.code(OWNER));

  await expect(page.getByText(`Signed in as ${OWNER} (Owner)`, { exact: false })).toBeVisible();
});

test("refuses a wrong password without a code step", async ({ page, context }) => {
  await fakeBackend(context);
  await page.goto("/");

  await logIn(page, OWNER, "not the placeholder");

  await expect(
    page.getByText("The username, password or code is wrong.", { exact: false }),
  ).toBeVisible();
  await expect(page.getByLabel("Code")).toHaveCount(0);
});
