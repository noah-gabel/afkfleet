// The first login's 2FA setup (Plan.md P8.9): the QR code and the key, the recovery codes
// shown once and copied, then the next code, in Edge, under the release CSP.

import { NEW_USER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { logIn, setUp2fa } from "./steps";

test("sets up 2FA at the first login, then logs in with the next code", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  await page.goto("/");

  await logIn(page, NEW_USER);
  const copied = await setUp2fa(page, backend, NEW_USER);

  expect(copied.split("\n")).toHaveLength(10);
  expect(copied).toContain(`rcvr-${NEW_USER}-0`);
  expect(backend.hasMfa(NEW_USER)).toBe(true);
});
