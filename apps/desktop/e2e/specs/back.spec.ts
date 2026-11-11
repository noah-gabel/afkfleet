// The universal Back control in the real engine (Plan.md P11.11; ADR-0020, U111, U112, S126): real
// keys, and the mouse's side-button as Edge reports it. A physical side-button in the real app is the
// user's hands-on: Playwright can send only left, middle and right, so these dispatch its events.

import type { Page } from "@playwright/test";

import { MEMBER, OWNER, PASSWORD } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";

const back = (page: Page) => page.getByRole("button", { name: "Back", exact: true });

/** The side-button, pressed and let go, on whatever's in the window. */
const sideButton = (page: Page) =>
  page.evaluate(() => {
    for (const type of ["mousedown", "mouseup", "auxclick"]) {
      document.body.dispatchEvent(
        new MouseEvent(type, { bubbles: true, cancelable: true, button: 3 }),
      );
    }
  });

/** Signed in as `username`, on Home. */
async function signIn(
  page: Page,
  backend: Awaited<ReturnType<typeof fakeBackend>>,
  username: string,
) {
  await page.goto("/");
  await logIn(page, username);
  await enterCode(page, backend.code(username));
  await expectHome(page, username);
}

test("Home to a bot and Back again, the heading in the same place on both", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  await signIn(page, backend, OWNER);
  await expect(back(page)).toHaveCount(0);
  const row = page.locator("[data-slot=back-row]");
  const rowBefore = await row.boundingBox();
  const bots = page.getByRole("heading", { name: "Bots", level: 1 });
  const headingBefore = await bots.boundingBox();

  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  await expect(page.getByRole("heading", { name: "AfkBot1", level: 1 })).toBeVisible();
  expect(await row.boundingBox()).toEqual(rowBefore);

  await back(page).click();

  await expect(bots).toBeVisible();
  await expect(back(page)).toHaveCount(0);
  expect(await bots.boundingBox()).toEqual(headingBefore);
  await expect(bots).toBeFocused();
  await expect(bots).toHaveCSS("outline-style", "none");
});

test("the side-button and Alt+Left each go back one screen", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  await signIn(page, backend, OWNER);
  const nav = page.getByRole("navigation", { name: "Main" });
  await nav.getByRole("link", { name: "Modes", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Modes", level: 1 })).toBeVisible();
  await nav.getByRole("link", { name: "Accounts", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Accounts", level: 1 })).toBeVisible();

  await sideButton(page);
  await expect(page.getByRole("heading", { name: "Modes", level: 1 })).toBeVisible();

  await page.keyboard.press("Alt+ArrowLeft");
  const bots = page.getByRole("heading", { name: "Bots", level: 1 });
  await expect(bots).toBeVisible();
  await expect(bots).toBeFocused();

  // Nothing more to go back to: both stay put, and the app stays where it is.
  await page.keyboard.press("Alt+ArrowLeft");
  await sideButton(page);
  await expect(page.getByRole("heading", { name: "Bots", level: 1 })).toBeVisible();
});

test("Back pressed with Enter leaves no ring on the heading it lands on", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  await signIn(page, backend, OWNER);
  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  await expect(back(page)).toBeVisible();

  // Chromium doesn't count Alt+Left as keyboard use, so only a plain key can show the ring.
  await back(page).focus();
  await page.keyboard.press("Enter");

  const bots = page.getByRole("heading", { name: "Bots", level: 1 });
  await expect(bots).toBeFocused();
  expect(await bots.evaluate((heading) => heading.matches(":focus-visible"))).toBe(true);
  await expect(bots).toHaveCSS("outline-style", "none");
});

test("across a logout and a login, Back never reaches the sign-in screens", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  await signIn(page, backend, OWNER);
  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  await expect(back(page)).toBeVisible();
  await page.getByRole("button", { name: "Log out" }).click();
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();

  await logIn(page, MEMBER, PASSWORD);
  await enterCode(page, backend.code(MEMBER));
  await expectHome(page, MEMBER);

  await expect(back(page)).toHaveCount(0);
  await page.keyboard.press("Alt+ArrowLeft");
  await sideButton(page);
  await expectHome(page, MEMBER);
});

test("after Leave, Back skips the gone account", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  const account = backend.addAccount(OWNER);
  backend.share(account, MEMBER, "view");
  await signIn(page, backend, MEMBER);
  const main = page.getByRole("main");
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Accounts", exact: true })
    .click();
  await main.getByRole("link", { name: /^(Open )?AfkBot1$/ }).click();
  await main.getByRole("button", { name: "Leave" }).click();
  await main.getByRole("button", { name: "Leave account" }).click();
  await expect(main.getByRole("heading", { name: "Accounts", level: 1 })).toBeVisible();

  await back(page).click();

  await expect(page.getByRole("heading", { name: "Bots", level: 1 })).toBeVisible();
  await expect(back(page)).toHaveCount(0);
});

test("a cancelled mode doesn't reopen on Back", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await signIn(page, backend, OWNER);
  const main = page.getByRole("main");
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Modes", exact: true })
    .click();
  await main.getByRole("link", { name: "New mode" }).click();
  await main.getByRole("button", { name: "Cancel" }).click();
  await expect(main.getByRole("heading", { name: "Modes", level: 1 })).toBeVisible();

  await back(page).click();

  await expect(page.getByRole("heading", { name: "Bots", level: 1 })).toBeVisible();
});
