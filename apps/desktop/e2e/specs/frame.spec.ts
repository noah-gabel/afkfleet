// The frame in a small window (Plan.md P11.11; ADR-0020, U91, S101, S102, S106), in Edge, as a
// Vitest has no layout: the app's minimum window holds the whole sidebar with the banner up, it
// collapses at 1000 px with every name kept, and a tooltip isn't clipped.

import { readFileSync } from "node:fs";

import type { Page } from "@playwright/test";

import { OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";

const tauri: { app: { windows: { minWidth: number; minHeight: number }[] } } = JSON.parse(
  readFileSync(new URL("../../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
);
const [main] = tauri.app.windows;
if (main === undefined) {
  throw new Error("tauri.conf.json has no window");
}

/** The Owner on Home: every entry there is. */
async function signedIn(page: Page, context: Parameters<typeof fakeBackend>[0]) {
  const backend = await fakeBackend(context);
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  return backend;
}

const sidebar = (page: Page) => page.locator("aside");
const nav = (page: Page) => page.getByRole("navigation", { name: "Main" });

test("the minimum window holds the whole sidebar, a certificate banner up", async ({
  page,
  context,
}) => {
  const backend = await signedIn(page, context);

  backend.refuseAll("certificate");
  await nav(page).getByRole("link", { name: "Modes" }).click();
  const banner = page.locator("[data-slot=connection-banner]");
  await expect(banner.getByRole("button", { name: "Server settings" })).toBeVisible();

  // The minimum window collapses the sidebar; the widest one at that height is its tallest form.
  for (const width of [main.minWidth, 1000]) {
    await page.setViewportSize({ width, height: main.minHeight });
    await expect
      .poll(async () => (await sidebar(page).boundingBox())?.width)
      .toBe(width < 1000 ? 64 : 208);

    await expect(nav(page).getByRole("link", { name: "Audit log" })).toBeInViewport({ ratio: 1 });
    await expect(page.getByRole("button", { name: "Log out" })).toBeInViewport({ ratio: 1 });
    const scrolls = await nav(page).evaluate((list) => list.scrollHeight > list.clientHeight);
    expect(scrolls, `the list scrolls at ${width} px`).toBe(false);
    const sideways = await page.evaluate(
      () => document.documentElement.scrollWidth > document.documentElement.clientWidth,
    );
    expect(sideways, `the page scrolls sideways at ${width} px`).toBe(false);

    const edge = (await sidebar(page).boundingBox())?.width;
    expect((await banner.boundingBox())?.x).toBe(edge);
  }
});

test("a sign-in step's heading takes the focus without looking focused", async ({
  page,
  context,
}) => {
  await fakeBackend(context);
  await page.goto("/");
  const heading = page.getByRole("heading", { name: "Log in" });
  await expect(heading).toBeFocused();

  await expect(heading).toHaveCSS("outline-style", "none");
});

test("the logo loads under the CSP, and the wordmark reads as one word", async ({
  page,
  context,
}) => {
  await signedIn(page, context);

  const logo = sidebar(page).locator("img");
  await expect(logo).toHaveAttribute("alt", "");
  await expect
    .poll(() => logo.evaluate((image: HTMLImageElement) => image.naturalWidth))
    .toBeGreaterThan(0);
  await expect(sidebar(page).locator("[data-slot=wordmark]")).toHaveText("afkfleet");
});

test("the banner runs the window's full width when signed out", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();

  backend.refuseAll("unreachable");
  await logIn(page, OWNER);
  const banner = page.locator("[data-slot=connection-banner]");
  await expect(banner.getByRole("button", { name: "Retry now" })).toBeVisible();

  expect((await banner.boundingBox())?.x).toBe(0);
});

test("the sidebar collapses under 1000 px, keeping each name and showing initials", async ({
  page,
  context,
}) => {
  await signedIn(page, context);

  await page.setViewportSize({ width: 1000, height: 700 });
  await expect.poll(async () => (await sidebar(page).boundingBox())?.width).toBe(208);
  await expect(page.getByRole("group", { name: "Account" }).getByText(OWNER)).toBeVisible();

  await page.setViewportSize({ width: 999, height: 700 });
  await expect.poll(async () => (await sidebar(page).boundingBox())?.width).toBe(64);

  for (const name of [
    "Home",
    "Modes",
    "Accounts",
    "Server",
    "Profile",
    "Users",
    "Invites",
    "Minecraft servers",
    "Agents",
    "Audit log",
  ]) {
    await expect(nav(page).getByRole("link", { name, exact: true })).toBeVisible();
  }
  const box = page.getByRole("group", { name: "Account" }).getByRole("link");
  await expect(box).toHaveAccessibleName(`${OWNER}, Owner, Profile`);
  await expect(box).toHaveText(new RegExp(OWNER.slice(0, 2), "i"));
  await expect(page.getByRole("button", { name: "Log out" })).toBeVisible();
});

test("a collapsed entry's tooltip shows its name, unclipped; a wide one's doesn't", async ({
  page,
  context,
}) => {
  await signedIn(page, context);
  const home = nav(page).getByRole("link", { name: "Home", exact: true });
  const tip = page.locator("[data-slot=tooltip]");

  await page.setViewportSize({ width: 1200, height: 700 });
  await home.hover();
  await expect(tip).toBeHidden();
  await page.mouse.move(700, 500);

  await page.setViewportSize({ width: 900, height: 700 });
  await home.hover();
  await expect(tip).toBeVisible();
  await expect(tip).toHaveText("Home");
  const onTop = await tip.evaluate((held) => {
    const box = held.getBoundingClientRect();
    const hit = document.elementFromPoint(box.right - 2, box.top + box.height / 2);
    return hit !== null && held.contains(hit);
  });
  expect(onTop).toBe(true);
  await page.mouse.move(700, 500);
  await expect(tip).toHaveCount(0);

  await page.keyboard.press("Tab");
  await nav(page).getByRole("link", { name: "Modes", exact: true }).focus();
  await page.keyboard.press("Shift+Tab");
  await expect(tip).toHaveText("Home");
  await page.keyboard.press("Escape");
  await expect(tip).toHaveCount(0);
});

test("Log out's tooltip shows in the wide form too", async ({ page, context }) => {
  await signedIn(page, context);
  await page.setViewportSize({ width: 1200, height: 700 });

  await page.getByRole("button", { name: "Log out" }).hover();

  await expect(page.locator("[data-slot=tooltip]")).toHaveText("Log out");
});
