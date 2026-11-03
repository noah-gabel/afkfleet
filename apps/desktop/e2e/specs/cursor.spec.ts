// The hand over every enabled control that acts on a click (Plan.md P11.11; ADR-0020, U114), read as
// the engine computes it: Tailwind v4's preflight gives buttons none, so a card's Start showed the
// arrow inside a card that showed the hand. A disabled control keeps the arrow.

import type { Locator } from "@playwright/test";

import { OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";

/** The cursor the engine shows over `locator`. */
const cursor = (locator: Locator) =>
  locator.evaluate((element) => getComputedStyle(element).cursor);

test("buttons, a card's Start, a select and a radio's label show the hand; a disabled button doesn't", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  const main = page.getByRole("main");

  expect(await cursor(page.getByRole("button", { name: "Log out" }))).toBe("pointer");
  expect(await cursor(main.getByRole("button", { name: "Start AfkBot1" }))).toBe("pointer");

  await main.getByRole("link", { name: "Open AfkBot1" }).click();
  expect(await cursor(main.getByRole("combobox", { name: "Mode" }))).toBe("pointer");
  backend.hold("start_bot");
  const start = main.getByRole("button", { name: "Start", exact: true });
  await start.click();
  await expect(start).toBeDisabled();
  expect(await cursor(start)).not.toBe("pointer");
  backend.release();

  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Profile" })
    .click();
  const dark = main.getByRole("radio", { name: "Dark" });
  expect(await cursor(dark)).toBe("pointer");
  expect(
    await dark.evaluate((input) =>
      input.parentElement === null ? "" : getComputedStyle(input.parentElement).cursor,
    ),
  ).toBe("pointer");
});
