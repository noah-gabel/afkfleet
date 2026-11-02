// Every corner comes from `--radius` (Plan.md P11.11; ADR-0020, U101): with it at 0, nothing on a
// sign-in screen or Profile is rounded but a real circle. The logo's tile lives inside its SVG, so
// it's no element's corner. scripts/theme-guard.mjs keeps the source on the scale; this proves the
// one line does what it promises.

import type { Page } from "@playwright/test";

import { MEMBER, NEW_USER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";

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
      // `rounded-full` computes to an enormous length, and a native radio to 50%: real circles.
      const circle = corners.every(
        (corner) => corner === "50%" || Number.parseFloat(corner) >= 9999,
      );
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
  const plant = (radius: string) =>
    page.evaluate((value) => {
      document.querySelector("[data-planted]")?.remove();
      const planted = document.createElement("div");
      planted.dataset.planted = "yes";
      planted.style.borderRadius = value;
      document.body.append(planted);
    }, radius);

  await plant("3px");
  expect((await stillRounded(page)).rounded).toHaveLength(1);
  await plant("calc(1px + 3px)");
  expect((await stillRounded(page)).rounded).toHaveLength(1);
  await plant("calc(infinity * 1px)");
  expect((await stillRounded(page)).rounded).toEqual([]);
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
