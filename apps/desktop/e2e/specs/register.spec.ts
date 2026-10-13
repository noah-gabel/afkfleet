// Registering with an invite, straight into the 2FA setup (Plan.md P8.9), in Edge, under the
// release CSP.

import { INVITE_CODE, PASSWORD } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { setUp2fa } from "./steps";

test("registers with an invite, sets up 2FA and lands on Home", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await page.goto("/");

  await page.getByRole("link", { name: "Register with an invite" }).click();
  await page.getByLabel("Invite code").fill(`  ${INVITE_CODE}\n`);
  await page.getByLabel("Username").fill("afkbot7");
  await page.getByLabel("Password", { exact: true }).fill(PASSWORD);
  await page.getByLabel("Repeat password", { exact: true }).fill(PASSWORD);
  await page.getByRole("button", { name: "Register" }).click();

  await setUp2fa(page, backend, "afkbot7");
  expect(backend.hasMfa("afkbot7")).toBe(true);
});
