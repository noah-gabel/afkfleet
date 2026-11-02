// The frame in a small window (Plan.md P11.11; ADR-0020, U91, S101, S102, S106), in Edge, as a
// Vitest has no layout: the app's minimum window holds the whole sidebar with the banner up, it
// collapses at 1000 px with every name kept, and a tooltip isn't clipped.

import { readFileSync } from "node:fs";

import type { Page } from "@playwright/test";

import { NEW_USER, OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";
import { inScheme, logoParts } from "./theme";

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

for (const screen of ["Home", "Modes", "Agents"] as const) {
  test(`on ${screen}, wide content scrolls in its column at the minimum window, never the page`, async ({
    page,
    context,
  }) => {
    await signedIn(page, context);
    await page.setViewportSize({ width: main.minWidth, height: main.minHeight });
    await nav(page).getByRole("link", { name: screen, exact: true }).click();
    const content = page.getByRole("main");
    await expect(content).toBeVisible();

    const seen = await content.evaluate((held) => {
      const wide = document.createElement("div");
      wide.style.width = "2000px";
      wide.style.height = "20px";
      held.append(wide);
      window.scrollBy(500, 500);
      const column = document.querySelector("[data-slot=content]");
      if (column === null) {
        throw new Error("no content column");
      }
      const start = column.getBoundingClientRect().left;
      column.scrollLeft = 500;
      const rail = document.querySelector("aside");
      const box = wide.getBoundingClientRect();
      const hit = rail === null ? null : document.elementFromPoint(30, box.top + 5);
      const read = {
        page: [window.scrollX, window.scrollY],
        sideways: document.documentElement.scrollWidth > document.documentElement.clientWidth,
        columnStart: start,
        railWidth: rail?.getBoundingClientRect().width,
        columnScrolled: column.scrollLeft,
        wideUnderRail: hit !== null && wide.contains(hit),
      };
      wide.remove();
      return read;
    });

    expect(seen.page).toEqual([0, 0]);
    expect(seen.sideways).toBe(false);
    expect(seen.columnStart).toBe(seen.railWidth);
    expect(seen.columnScrolled).toBe(500);
    expect(seen.wideUnderRail).toBe(false);
  });
}

test("the column goes back to its top on a new screen, and the page itself never scrolls", async ({
  page,
  context,
}) => {
  await signedIn(page, context);
  await page.setViewportSize({ width: main.minWidth, height: main.minHeight });
  const scrolled = await page.evaluate(() => {
    const column = document.querySelector("[data-slot=content]");
    if (column === null) {
      throw new Error("no content column");
    }
    // In the column, not the screen, so it outlives the navigation: with nothing to scroll on the
    // next screen, the column would read 0 whether or not anything reset it.
    const tall = document.createElement("div");
    tall.style.height = "3000px";
    tall.style.flexShrink = "0";
    column.append(tall);
    column.scrollTop = 1000;
    return {
      column: column.scrollTop,
      page: document.documentElement.scrollHeight > document.documentElement.clientHeight,
    };
  });
  expect(scrolled).toEqual({ column: 1000, page: false });

  await nav(page).getByRole("link", { name: "Modes", exact: true }).click();
  await expect(page.getByRole("main").getByRole("link", { name: "New mode" })).toBeVisible();

  await expect
    .poll(() => page.locator("[data-slot=content]").evaluate((column) => column.scrollTop))
    .toBe(0);
});

test("2FA setup's top can be reached at the minimum window", async ({ page, context }) => {
  await fakeBackend(context);
  await page.setViewportSize({ width: main.minWidth, height: main.minHeight });
  await page.goto("/");
  await logIn(page, NEW_USER);
  await expect(page.getByRole("img", { name: /QR code/ })).toBeVisible();

  const seen = await page.evaluate(() => {
    const heading = document.querySelector("h1");
    const scroller = document.querySelector("[data-slot=screen]");
    if (heading === null || scroller === null) {
      throw new Error("no heading or scroller");
    }
    scroller.scrollTop = 0;
    return {
      top: heading.getBoundingClientRect().top,
      scrolls: scroller.scrollHeight > scroller.clientHeight,
      page: document.documentElement.scrollHeight > document.documentElement.clientHeight,
    };
  });

  expect(seen.top).toBeGreaterThanOrEqual(0);
  expect(seen.scrolls).toBe(true);
  expect(seen.page).toBe(false);

  // The real wheel: a script can set `scrollTop` on a box that only clips.
  await page.getByRole("img", { name: /QR code/ }).hover();
  await page.mouse.wheel(0, 200);
  await expect
    .poll(() => page.locator("[data-slot=screen]").evaluate((scroller) => scroller.scrollTop))
    .toBeGreaterThan(0);
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

test("the logo's three parts read on a sign-in card and the sidebar, in both themes", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
  const holds = async (where: string) => {
    for (const scheme of ["dark", "light"] as const) {
      await inScheme(page, scheme);
      const parts = await logoParts(page);
      expect(parts, `${where} in ${scheme}`).toHaveLength(3);
      for (const part of parts) {
        expect(part.ratio, `${where} in ${scheme}: ${part.color}`).toBeGreaterThanOrEqual(3);
      }
      const [ring, , satellite] = parts;
      expect(satellite?.ratio, `${where} in ${scheme}`).toBeGreaterThan(ring?.ratio ?? 0);
    }
  };

  await holds("the sign-in card");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await holds("the sidebar");
});

test("the wordmark reads as one word", async ({ page, context }) => {
  await signedIn(page, context);

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
    // It takes no pointer, so hit-testing needs it to for this one read.
    held.style.pointerEvents = "auto";
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

for (const [form, width] of [
  ["wide", 1200],
  ["collapsed", 900],
] as const) {
  test(`in the ${form} sidebar, hovering the box then moving onto Log out, one click logs out`, async ({
    page,
    context,
  }) => {
    await signedIn(page, context);
    await page.setViewportSize({ width, height: 700 });
    const account = page.getByRole("group", { name: "Account" });
    const center = async (locator: ReturnType<Page["locator"]>) => {
      const box = await locator.boundingBox();
      if (box === null) {
        throw new Error("nothing to hover");
      }
      return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
    };
    const box = await center(account.getByRole("link"));
    const logOut = await center(account.getByRole("button", { name: "Log out" }));

    // The raw mouse: `hover()` and `click()` would wait for a clear shot and hide what a tip covers.
    await page.mouse.move(box.x, box.y);
    await page.mouse.move(logOut.x, logOut.y, { steps: 5 });
    await page.mouse.click(logOut.x, logOut.y);

    await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
  });
}

test("the wide box shows its name and role, so it has no tip", async ({ page, context }) => {
  await signedIn(page, context);
  await page.setViewportSize({ width: 1200, height: 700 });

  await page.getByRole("group", { name: "Account" }).getByRole("link").hover();

  await expect(page.locator("[data-slot=tooltip]")).toBeHidden();
});

test("no tip covers a control or takes the pointer, in either form", async ({ page, context }) => {
  await signedIn(page, context);
  const tip = page.locator("[data-slot=tooltip]");
  const account = page.getByRole("group", { name: "Account" });

  for (const [width, height, controls] of [
    [1200, 700, [account.getByRole("button", { name: "Log out" })]],
    [
      main.minWidth,
      main.minHeight,
      [
        nav(page).getByRole("link", { name: "Home", exact: true }),
        nav(page).getByRole("link", { name: "Audit log", exact: true }),
        account.getByRole("link"),
        account.getByRole("button", { name: "Log out" }),
      ],
    ],
  ] as const) {
    await page.setViewportSize({ width, height });
    for (const control of controls) {
      await control.hover();
      await expect(tip).toBeVisible();

      const seen = await tip.evaluate((held) => {
        const box = held.getBoundingClientRect();
        const covered = [...document.querySelectorAll("aside a, aside button")].flatMap((other) => {
          const rect = other.getBoundingClientRect();
          const apart =
            rect.right <= box.left ||
            rect.left >= box.right ||
            rect.bottom <= box.top ||
            rect.top >= box.bottom;
          return apart ? [] : [other.getAttribute("aria-label") ?? other.textContent];
        });
        const middle = { x: box.left + box.width / 2, y: box.top + box.height / 2 };
        const hit = document.elementFromPoint(middle.x, middle.y);
        return {
          covered,
          inWindow:
            box.left >= 0 &&
            box.top >= 0 &&
            box.right <= window.innerWidth &&
            box.bottom <= window.innerHeight,
          takesPointer: hit !== null && held.contains(hit),
          middle,
        };
      });

      expect(seen.covered, `at ${width} px`).toEqual([]);
      expect(seen.inWindow, `at ${width} px`).toBe(true);
      expect(seen.takesPointer, `at ${width} px`).toBe(false);
      await page.mouse.move(seen.middle.x, seen.middle.y);
      await expect(tip).toHaveCount(0);
    }
  }
});
