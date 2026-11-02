// Every corner comes from `--radius` (Plan.md P11.11; ADR-0020, U101): with it at 0, nothing on a
// sign-in screen or Profile is rounded but a real circle. scripts/theme-guard.mjs keeps the source on
// the scale; this proves the one line does what it promises.

import type { Page } from "@playwright/test";

import { MEMBER, NEW_USER, OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn, push } from "./steps";

/** Every element on the page with `--radius: 0` that's still rounded, and how many it read. */
async function stillRounded(page: Page): Promise<{ read: number; rounded: readonly string[] }> {
  return page.evaluate(() => {
    const root = document.documentElement;
    root.style.setProperty("--radius", "0");
    // A button's `transition-all` would otherwise read as it starts.
    for (const animation of document.getAnimations()) {
      if (animation.effect?.getComputedTiming().endTime !== Number.POSITIVE_INFINITY) {
        animation.finish();
      }
    }
    const elements = [...document.body.querySelectorAll("*")];
    const rounded = elements.flatMap((element) => {
      const style = getComputedStyle(element);
      const corners = [
        style.borderTopLeftRadius,
        style.borderTopRightRadius,
        style.borderBottomLeftRadius,
        style.borderBottomRightRadius,
      ];
      // `rounded-full` computes to an enormous length, and a native radio to 50%: a real circle
      // only on a square box, as a pill that can read "99+" is no circle.
      const box = element.getBoundingClientRect();
      const circle =
        Math.abs(box.width - box.height) <= 0.5 &&
        corners.every((corner) => corner === "50%" || Number.parseFloat(corner) >= 9999);
      return corners.every((corner) => Number.parseFloat(corner) === 0) || circle
        ? []
        : [`<${element.tagName.toLowerCase()}> ${element.className} ${corners.join(" ")}`];
    });
    root.style.removeProperty("--radius");
    return { read: elements.length, rounded };
  });
}

/** Holds what's on the page, naming `screen` when it fails. */
async function squared(page: Page, screen: string): Promise<void> {
  const { read, rounded } = await stillRounded(page);
  expect(read, `${screen}: nothing was read`).toBeGreaterThan(5);
  expect(rounded, screen).toEqual([]);
}

test("the check sees a rounded corner, and passes a real circle", async ({ page, context }) => {
  await fakeBackend(context);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
  const plant = (radius: string, width = "20px") =>
    page.evaluate(
      ([value, across]) => {
        document.querySelector("[data-planted]")?.remove();
        const planted = document.createElement("div");
        planted.dataset.planted = "yes";
        planted.style.borderRadius = value ?? "";
        planted.style.width = across ?? "";
        planted.style.height = "20px";
        document.body.append(planted);
      },
      [radius, width],
    );

  await plant("3px");
  expect((await stillRounded(page)).rounded).toHaveLength(1);
  await plant("calc(1px + 3px)");
  expect((await stillRounded(page)).rounded).toHaveLength(1);
  await plant("calc(infinity * 1px)");
  expect((await stillRounded(page)).rounded).toEqual([]);
  // A pill isn't a circle: it stays round whatever `--radius` is.
  await plant("calc(infinity * 1px)", "40px");
  expect((await stillRounded(page)).rounded).toHaveLength(1);
  await plant("var(--radius)");
  expect((await stillRounded(page)).rounded).toEqual([]);
  await page.evaluate(() => document.querySelector("[data-planted]")?.remove());
});

test("--radius: 0 squares every sign-in screen", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
  await squared(page, "login");

  await page.getByRole("link", { name: "Register with an invite" }).click();
  await expect(page.getByRole("heading", { name: "Register" })).toBeVisible();
  await squared(page, "register");
  await page.goto("/");

  await page.getByRole("link", { name: "Server settings" }).click();
  await expect(page.getByLabel(/address/i).first()).toBeVisible();
  await squared(page, "server settings");
  await page.goto("/");

  await logIn(page, MEMBER);
  await expect(page.getByLabel("Code")).toBeVisible();
  await squared(page, "the code step");
  await page.goto("/");

  await logIn(page, NEW_USER);
  await expect(page.getByRole("img", { name: /QR code/ })).toBeVisible();
  await squared(page, "2FA setup");
  await enterCode(page, backend.setupCode());
  await expect(page.getByRole("list", { name: "Recovery codes" })).toBeVisible();
  await squared(page, "recovery codes");
});

test("--radius: 0 squares Profile and the frame around it", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await page.goto("/");
  await logIn(page, MEMBER);
  await enterCode(page, backend.code(MEMBER));
  await expectHome(page, MEMBER);
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Profile" })
    .click();
  await expect(page.getByRole("group", { name: "Theme" })).toBeVisible();

  await squared(page, "Profile");
});

test("--radius: 0 squares Home, a bot, Accounts and an account, the count's pill included", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  const account = backend.addAccount(OWNER);
  backend.share(account, MEMBER, "view");
  backend.addAccount(MEMBER, "pending");
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  const main = page.getByRole("main");
  await expect(page.getByRole("link", { name: "Open AfkBot1" })).toBeVisible();
  await expect(page.getByRole("link", { name: /^Accounts, / })).toBeVisible();
  await squared(page, "Home");

  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  await expect(main.getByRole("region", { name: "Settings" })).toBeVisible();
  await squared(page, "a bot");

  await page.getByRole("link", { name: /^Accounts, / }).click();
  await expect(main.getByRole("heading", { name: "Accounts", level: 1 })).toBeVisible();
  await squared(page, "Accounts");

  await main
    .getByRole("link", { name: /^(Open )?AfkBot1$/ })
    .first()
    .click();
  await expect(main.getByRole("table", { name: "Who has access" })).toBeVisible();
  await squared(page, "an account");
});

test("--radius: 0 squares a toast and its action", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await push(page, {
    update: "event",
    event: {
      type: "bot_died",
      bot_id: backend.bot("AfkBot1").id,
      at: new Date().toISOString(),
    },
  });
  const toast = page.locator("[data-sonner-toast]");
  await expect(toast.getByRole("button", { name: "Open" })).toBeVisible();
  await toast.hover();

  await squared(page, "a toast with an action");
});
