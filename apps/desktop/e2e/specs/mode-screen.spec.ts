// A mode's screen in the real engine (Plan.md P11.11; ADR-0020, U120, R232), as jsdom has no layout:
// two columns as a bot's screen has them, from 912 px on the rail and beside the wide sidebar, each
// scrolling inside itself so Save stays in view; stacked, Save and Cancel in a bar at the column's
// foot that never lies over the last step once it's scrolled to; and the Tab order the order on the
// screen. The scrollbars show, as WebView2 shows them.
import type { Page } from "@playwright/test";

import { type FakeBackend, OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";
import { frameHolds } from "./theme";

test.use({ launchOptions: { ignoreDefaultArgs: ["--hide-scrollbars"] } });

/** Sixteen steps over two sections: more than a window holds. */
const STEPS = Array.from({ length: 16 }, (_, n) =>
  n < 6
    ? {
        action: { type: "jump" as const },
        schedule: { type: "at_start" as const },
        probability: 100,
      }
    : {
        action: { type: "swing_arm" as const },
        schedule: { type: "every" as const, interval_ms: 1_000 * (n + 1), jitter_ms: 0 },
        probability: 100,
      },
);

/** The Owner on their mode "Long", sixteen steps, opened from the list. */
async function atLongMode(
  page: Page,
  context: Parameters<typeof fakeBackend>[0],
): Promise<FakeBackend> {
  const backend = await fakeBackend(context);
  backend.addMode(OWNER, "Long", STEPS);
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Modes" }).click();
  await page.getByRole("link", { name: "Open Long" }).click();
  await expect(page.getByRole("heading", { name: "Long", level: 1 })).toBeVisible();
  return backend;
}

/** Where the editor's parts are, and what scrolls. */
const parts = (page: Page) =>
  page.evaluate(() => {
    const box = (slot: string) =>
      document.querySelector(`[data-slot=${slot}]`)?.getBoundingClientRect();
    const scrolls = (slot: string) => {
      const element = document.querySelector(`[data-slot=${slot}]`);
      return element !== null && element.scrollHeight > element.clientHeight;
    };
    const editor = box("editor-box");
    const sections = box("editor-sections");
    const steps = [...document.querySelectorAll("[data-slot=editor-sections] fieldset fieldset")];
    return {
      beside: editor !== undefined && sections !== undefined && sections.left >= editor.right,
      contentScrolls: scrolls("content"),
      sectionsScroll: scrolls("editor-sections"),
      bar: box("editor-actions"),
      content: box("content"),
      lastStep: steps.at(-1)?.getBoundingClientRect(),
      apart: box("editor-apart"),
    };
  });

for (const [width, beside] of [
  [800, false],
  [911, false],
  [912, true],
  [1055, true],
  [1056, true],
  [1800, true],
] as const) {
  test(`at ${width} px the editor is ${beside ? "two columns" : "stacked"}`, async ({
    page,
    context,
  }) => {
    await atLongMode(page, context);

    await page.setViewportSize({ width, height: 800 });

    await expect.poll(async () => (await parts(page)).beside).toBe(beside);
  });
}

test("in two columns the steps scroll in their own column, and Save stays in view", async ({
  page,
  context,
}) => {
  await page.setViewportSize({ width: 1280, height: 720 });
  await atLongMode(page, context);
  const save = page.getByRole("button", { name: "Save", exact: true });

  const at = await parts(page);
  await page.locator("[data-slot=editor-sections]").evaluate((column) => {
    column.scrollTop = column.scrollHeight;
  });

  expect(at).toMatchObject({ beside: true, contentScrolls: false, sectionsScroll: true });
  await expect(save).toBeInViewport({ ratio: 1 });
  await frameHolds(page, "the editor in two columns");
});

test("stacked, Save and Cancel stay at the column's foot, and never over the last step", async ({
  page,
  context,
}) => {
  await page.setViewportSize({ width: 800, height: 600 });
  await atLongMode(page, context);
  const save = page.getByRole("button", { name: "Save", exact: true });

  const top = await parts(page);
  await expect(save).toBeInViewport({ ratio: 1 });
  await page.locator("[data-slot=content]").evaluate((column) => {
    column.scrollTop = column.scrollHeight;
  });
  const end = await parts(page);

  expect(top).toMatchObject({ beside: false, contentScrolls: true });
  // At the top it sits on the column's foot, over what's still below.
  expect(Math.abs((top.bar?.bottom ?? 0) - (top.content?.bottom ?? 0))).toBeLessThanOrEqual(1);
  // At the end it follows the last step, and a copy and Delete follow it.
  expect(end.bar?.top ?? 0).toBeGreaterThanOrEqual(
    end.lastStep?.bottom ?? Number.POSITIVE_INFINITY,
  );
  expect(end.apart?.top ?? 0).toBeGreaterThanOrEqual(end.bar?.bottom ?? Number.POSITIVE_INFINITY);
  await frameHolds(page, "the editor stacked");
});

test("stacked, the Tab order is the order on the screen: the steps before the bar", async ({
  page,
  context,
}) => {
  await page.setViewportSize({ width: 800, height: 600 });
  await atLongMode(page, context);
  await page.getByLabel("Name").focus();

  const seen: string[] = [];
  // Save is disabled until something changes, so the bar is found by Cancel.
  for (let press = 0; press < 200 && !seen.includes("bar"); press += 1) {
    await page.keyboard.press("Tab");
    seen.push(
      await page.evaluate(() => {
        const focused = document.activeElement;
        if (focused?.closest("[data-slot=editor-sections]") !== null) {
          return "step";
        }
        return focused?.closest("[data-slot=editor-actions]") === null ? "other" : "bar";
      }),
    );
  }

  expect(seen.indexOf("step")).toBeGreaterThanOrEqual(0);
  expect(seen.indexOf("step")).toBeLessThan(seen.indexOf("bar"));
  expect(seen.slice(seen.indexOf("bar"))).not.toContain("step");
});

test("a locked mode has the same columns and no bar", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await page.setViewportSize({ width: 800, height: 600 });
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Modes" }).click();
  await page.getByRole("link", { name: "Open afk" }).click();
  await expect(page.getByRole("button", { name: "Make a copy" })).toBeVisible();

  expect((await parts(page)).bar).toBeUndefined();
  await page.setViewportSize({ width: 1280, height: 720 });
  await expect.poll(async () => (await parts(page)).beside).toBe(true);
});
