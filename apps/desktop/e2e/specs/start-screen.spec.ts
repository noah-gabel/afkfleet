// The start screen's way out while the saved server doesn't answer (ADR-0022, S1, U7), in Edge,
// under the release CSP: the server settings, and a logout on this PC that says the server
// wasn't told.

import type { Page } from "@playwright/test";

import { type FakeBackend, OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";

/** Signs the Owner in, then starts the app again while the server doesn't answer. */
async function waitingSignedIn(page: Page, backend: FakeBackend): Promise<void> {
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  backend.refuseAll("unreachable");
  await page.reload();
  await expect(page.getByText("Waiting for the server…")).toBeVisible();
}

test("logs out on this PC while the server doesn't answer", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await waitingSignedIn(page, backend);

  await page.getByRole("button", { name: "Log out on this PC" }).click();

  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
  await expect(page.getByText(/^Logged out here\. Server not told/)).toBeVisible();
  expect(backend.timesCalled("logout")).toBe(1);
  // The session is gone: once the server answers, the app asks for a login, not Home.
  backend.refuseAll(null);
  await page.reload();
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
});

test("opens the server settings while the server doesn't answer", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await waitingSignedIn(page, backend);

  await page.getByRole("button", { name: "Server settings" }).click();

  await expect(page.getByRole("heading", { name: "Server setup" })).toBeVisible();
  expect(backend.timesCalled("logout")).toBe(0);
});

test("goes back from the server settings to the wait, then to Home once the server answers", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  await waitingSignedIn(page, backend);

  await page.getByRole("button", { name: "Server settings" }).click();
  await expect(page.getByRole("heading", { name: "Server setup" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Back to login" })).toHaveCount(0);
  await page.getByRole("link", { name: "Back", exact: true }).click();

  await expect(page.getByText("Waiting for the server…")).toBeVisible();
  // The session is still stored: once the server answers, Back leads Home.
  await page.getByRole("button", { name: "Server settings" }).click();
  await expect(page.getByRole("heading", { name: "Server setup" })).toBeVisible();
  backend.refuseAll(null);
  await page.getByRole("link", { name: "Back", exact: true }).click();
  await expectHome(page, OWNER);
  expect(backend.timesCalled("logout")).toBe(0);
});
