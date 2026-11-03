// A bot's screen in the real engine (Plan.md P11.11; ADR-0020, U39, U91, U92, U105, U113, U115, S123):
// where its two columns are, as jsdom has no layout. Widening the window goes stacked, then two
// columns, and never back: below 912 stacked on the rail, 912 to 1055 two columns on the rail, from
// 1056 two columns with the wide sidebar, whether the column had a scrollbar or not. The console
// fills the height, banner up or not, and nothing scrolls into blank space.
//
// This file runs with the scrollbars Playwright hides by default, as WebView2 shows them: a
// classic scrollbar narrows the column it scrolls, which the switch must not depend on (PR #90's
// review).

import type { Page } from "@playwright/test";

import { type FakeBackend, MEMBER, OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";
import { inScheme } from "./theme";

test.use({ launchOptions: { ignoreDefaultArgs: ["--hide-scrollbars"] } });

/** The Owner on bot 1's screen. */
async function atBot(page: Page, context: Parameters<typeof fakeBackend>[0]): Promise<FakeBackend> {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  backend.addChat("AfkBot1", {
    at: new Date().toISOString(),
    direction: "in",
    kind: "system",
    text: "Hello from the server",
    truncated: false,
    arguments_hidden: false,
    mine: false,
  });
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  await expect(page.getByText("Hello from the server")).toBeVisible();
  return backend;
}

const chat = (page: Page) => page.getByRole("main").getByRole("region", { name: "Chat" });
const settings = (page: Page) => page.getByRole("main").getByRole("region", { name: "Settings" });

/** Whether the chat stands beside the boxes, not under them. */
async function sideBySide(page: Page): Promise<boolean> {
  const [boxes, console] = await Promise.all([
    settings(page).boundingBox(),
    chat(page).boundingBox(),
  ]);
  if (boxes === null || console === null) {
    throw new Error("no boxes or chat");
  }
  return console.x >= boxes.x + boxes.width;
}

const railWidth = async (page: Page) => (await page.locator("aside").boundingBox())?.width;

for (const [width, beside, rail] of [
  [800, false, 64],
  [911, false, 64],
  [912, true, 64],
  [1055, true, 64],
  [1056, true, 208],
  [1200, true, 208],
] as const) {
  test(`at ${width} px it's ${beside ? "two columns" : "stacked"}, the sidebar ${rail} px`, async ({
    page,
    context,
  }) => {
    await atBot(page, context);

    await page.setViewportSize({ width, height: 700 });

    await expect.poll(() => railWidth(page)).toBe(rail);
    await expect.poll(() => sideBySide(page)).toBe(beside);
  });
}

for (const [width, rail] of [
  [912, 64],
  [1056, 208],
] as const) {
  test(`at ${width} px it's two columns even coming from stacked, its scrollbar shown`, async ({
    page,
    context,
  }) => {
    await atBot(page, context);
    await page.setViewportSize({ width: 800, height: 600 });
    await expect.poll(() => sideBySide(page)).toBe(false);
    const column = page.locator("[data-slot=content]");
    expect(await column.evaluate((held) => held.offsetWidth - held.clientWidth)).toBeGreaterThan(0);

    await page.setViewportSize({ width, height: 600 });

    await expect.poll(() => railWidth(page)).toBe(rail);
    await expect.poll(() => sideBySide(page)).toBe(true);
  });
}

/** Up the certificate banner over the bot's screen, as a refusing server does. */
async function withBanner(page: Page, backend: FakeBackend): Promise<void> {
  backend.refuseAll("certificate");
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Modes" }).click();
  await expect(
    page.locator("[data-slot=connection-banner]").getByRole("button", { name: "Server settings" }),
  ).toBeVisible();
  // Still refusing, so the banner stays up over the bot its cache still holds.
  await page.getByRole("button", { name: "Back", exact: true }).click();
  await expect(chat(page)).toBeVisible();
}

/** The column scrolled to its end: what scrolls, and the room below the screen's last element. */
const atTheEnd = (page: Page) =>
  page.locator("[data-slot=content]").evaluate((column) => {
    column.scrollTop = column.scrollHeight;
    const main = column.querySelector("main");
    const grid = main?.firstElementChild;
    const left = grid?.firstElementChild;
    const chat = main?.querySelector("section[aria-label=Chat]");
    const log = chat?.querySelector("[role=log]");
    if (main === null || main === undefined || !(left instanceof HTMLElement) || !chat || !log) {
      throw new Error("no screen to measure");
    }
    let last = 0;
    for (const element of main.querySelectorAll("*")) {
      const box = element.getBoundingClientRect();
      if (box.height > 0) {
        last = Math.max(last, box.bottom);
      }
    }
    const bottom = column.getBoundingClientRect().bottom;
    return {
      columnScrolls: column.scrollHeight > column.clientHeight,
      leftScrolls: getComputedStyle(left).overflowY === "auto",
      logScrolls: getComputedStyle(log).overflowY === "auto",
      padding: Number.parseFloat(getComputedStyle(main).paddingBottom),
      belowLast: Math.round(bottom - last),
      belowConsole: Math.round(bottom - chat.getBoundingClientRect().bottom),
    };
  });

for (const [width, height] of [
  [800, 600],
  [1000, 700],
  [1400, 900],
] as const) {
  for (const banner of [false, true]) {
    test(`at ${width}x${height}${banner ? " under the banner" : ""}, nothing scrolls into blank space`, async ({
      page,
      context,
    }) => {
      const backend = await atBot(page, context);
      await page.setViewportSize({ width, height });
      if (banner) {
        await withBanner(page, backend);
      }
      const beside = await sideBySide(page);

      const end = await atTheEnd(page);

      if (beside) {
        // Each column scrolls by itself, and the column around them not at all.
        expect(end).toMatchObject({ columnScrolls: false, leftScrolls: true, logScrolls: true });
        expect(end.belowLast).toBeGreaterThanOrEqual(0);
        expect(end.belowLast).toBeLessThanOrEqual(end.padding);
      } else {
        // The scroll ends at the console's foot and the screen's padding, nothing after it.
        expect(end.belowConsole).toBe(end.padding);
        expect(end.belowLast).toBe(end.padding);
      }
      backend.refuseAll(null);
    });
  }
}

test("the console fills the height beside the boxes, banner up or not", async ({
  page,
  context,
}) => {
  const backend = await atBot(page, context);
  await page.setViewportSize({ width: 1200, height: 800 });
  const send = page.getByRole("button", { name: "Send" });
  await expect(send).toBeInViewport({ ratio: 1 });
  const clear = await chat(page).boundingBox();
  // To the window's foot, less the screen's padding.
  expect((clear?.y ?? 0) + (clear?.height ?? 0)).toBeGreaterThan(800 - 32);
  const column = page.locator("[data-slot=content]");
  expect(await column.evaluate((held) => held.scrollHeight <= held.clientHeight)).toBe(true);

  backend.refuseAll("certificate");
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Modes" }).click();
  await expect(
    page.locator("[data-slot=connection-banner]").getByRole("button", { name: "Server settings" }),
  ).toBeVisible();
  // Still refusing, so the banner stays up over the bot its cache still holds.
  await page.getByRole("button", { name: "Back", exact: true }).click();
  await expect(chat(page)).toBeVisible();

  await expect(send).toBeInViewport({ ratio: 1 });
  const under = await chat(page).boundingBox();
  expect(under?.height).toBeLessThan(clear?.height ?? 0);
  expect((under?.y ?? 0) + (under?.height ?? 0)).toBeGreaterThan(800 - 32);
  backend.refuseAll(null);
});

test("a long left column scrolls by itself, and the console stays", async ({ page, context }) => {
  await atBot(page, context);
  await page.setViewportSize({ width: 1200, height: 500 });
  const before = await chat(page).boundingBox();

  const read = await settings(page).evaluate((box) => {
    const tall = document.createElement("div");
    tall.style.height = "2000px";
    tall.style.flexShrink = "0";
    box.after(tall);
    const left = box.parentElement;
    const column = document.querySelector("[data-slot=content]");
    if (left === null || column === null) {
      throw new Error("no left column");
    }
    left.scrollTop = 500;
    return {
      left: left.scrollTop,
      column: column.scrollTop,
      columnScrolls: column.scrollHeight > column.clientHeight,
    };
  });

  expect(read).toEqual({ left: 500, column: 0, columnScrolls: false });
  expect(await chat(page).boundingBox()).toEqual(before);
});

test("stacked at the minimum window, the console is below the boxes, a scroll away", async ({
  page,
  context,
}) => {
  await atBot(page, context);
  await page.setViewportSize({ width: 800, height: 600 });
  await expect.poll(() => sideBySide(page)).toBe(false);

  const box = await chat(page).boundingBox();
  expect(box?.height).toBeCloseTo(420, -1);
  await page.getByRole("button", { name: "Send" }).scrollIntoViewIfNeeded();
  await expect(page.getByRole("button", { name: "Send" })).toBeInViewport({ ratio: 1 });
});

test("the console takes its own colours, light in light mode and dark in dark", async ({
  page,
  context,
}) => {
  await atBot(page, context);
  const log = chat(page).getByRole("log");

  for (const [scheme, bg, fg] of [
    ["light", "rgb(227, 230, 234)", "rgb(22, 25, 28)"],
    ["dark", "rgb(10, 12, 13)", "rgb(228, 231, 234)"],
  ] as const) {
    await inScheme(page, scheme);
    await expect(log).toHaveCSS("background-color", bg);
    await expect(log).toHaveCSS("color", fg);
  }
  await expect(log).toHaveCSS("font-family", /Plex Mono/);
});

test("a View holder's screen has its boxes and the console, with no Settings", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  const account = backend.addAccount(OWNER);
  backend.share(account, MEMBER, "view");
  await page.goto("/");
  await logIn(page, MEMBER);
  await enterCode(page, backend.code(MEMBER));
  await expectHome(page, MEMBER);
  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  await expect(chat(page)).toBeVisible();

  await expect(settings(page)).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Send" })).toHaveCount(0);
  await expect(page.getByRole("region", { name: "Timeline" })).toBeVisible();
});
