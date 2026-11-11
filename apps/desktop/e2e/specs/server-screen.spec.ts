// A server's settings in the real engine (Plan.md P11.11; ADR-0020, U121, R232): the mode editor's
// columns, the texts and the formats scrolling on their own beside the address with Save in view,
// and stacked, Save and Cancel in a bar at the column's foot. The scrollbars show, as WebView2
// shows them.
import type { Locator, Page } from "@playwright/test";

import { type FakeBackend, OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";
import { frameHolds } from "./theme";

test.use({ launchOptions: { ignoreDefaultArgs: ["--hide-scrollbars"] } });

/** The Owner on a server's settings holding sixteen texts and eight formats. */
async function atLongSettings(
  page: Page,
  context: Parameters<typeof fakeBackend>[0],
): Promise<FakeBackend> {
  const backend = await fakeBackend(context);
  backend.addServerSettings(
    "long.example.com",
    Array.from({ length: 16 }, (_, n) => `You logged in from another location (${n + 1})`),
    Array.from({ length: 8 }, (_, n) => `[Clan ${n + 1}] {sender} : `),
  );
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Minecraft servers" })
    .click();
  await page.getByRole("link", { name: "Open long.example.com:25565" }).click();
  await expect(
    page.getByRole("heading", { name: "long.example.com:25565", level: 1 }),
  ).toBeVisible();
  return backend;
}

/** Whether the parts sit side by side, and what scrolls. */
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
    return {
      beside: editor !== undefined && sections !== undefined && sections.left >= editor.right,
      contentScrolls: scrolls("content"),
      sectionsScroll: scrolls("editor-sections"),
      bar: box("editor-actions"),
      content: box("content"),
    };
  });

test("in two columns the texts and formats scroll on their own, and Save stays in view", async ({
  page,
  context,
}) => {
  await page.setViewportSize({ width: 1280, height: 720 });
  await atLongSettings(page, context);

  const at = await parts(page);
  await page.locator("[data-slot=editor-sections]").evaluate((column) => {
    column.scrollTop = column.scrollHeight;
  });

  expect(at).toMatchObject({ beside: true, contentScrolls: false, sectionsScroll: true });
  await expect(page.getByRole("button", { name: "Save", exact: true })).toBeInViewport({
    ratio: 1,
  });
  await expect(page.getByRole("button", { name: "Try", exact: true })).toBeInViewport();
  await frameHolds(page, "a server's settings in two columns");
});

test("stacked, Save and Cancel stay at the column's foot", async ({ page, context }) => {
  await page.setViewportSize({ width: 800, height: 600 });
  await atLongSettings(page, context);

  const top = await parts(page);

  expect(top).toMatchObject({ beside: false, contentScrolls: true });
  expect(Math.abs((top.bar?.bottom ?? 0) - (top.content?.bottom ?? 0))).toBeLessThanOrEqual(1);
  await expect(page.getByRole("button", { name: "Save", exact: true })).toBeInViewport({
    ratio: 1,
  });
  await frameHolds(page, "a server's settings stacked");
});

/** Where `target` is: whole inside the content column, and clear of the stacked bar. */
const where = (target: Locator) =>
  target.evaluate((element) => {
    const box = element.getBoundingClientRect();
    const column = document.querySelector("[data-slot=content]")?.getBoundingClientRect();
    const bar = document.querySelector("[data-slot=editor-actions]")?.getBoundingClientRect();
    return {
      inColumn:
        column !== undefined && box.top >= column.top - 1 && box.bottom <= column.bottom + 1,
      underBar: bar !== undefined && box.bottom > bar.top + 1 && box.top < bar.bottom - 1,
    };
  });

test("stacked, a save asks for no password, and changed settings show where you are", async ({
  page,
  context,
}) => {
  await page.setViewportSize({ width: 800, height: 600 });
  const backend = await atLongSettings(page, context);
  const main = page.getByRole("main");
  const save = main.getByRole("button", { name: "Save", exact: true });
  const toTop = () =>
    page.locator("[data-slot=content]").evaluate((column) => {
      column.scrollTop = 0;
    });

  await save.click();
  await expect(page.getByText("Saved. Bots on it use the change at once.")).toBeVisible();
  await expect(page.getByLabel("Current password")).toHaveCount(0);
  // The toast slides in from below the window: it must be gone before the frame is checked.
  await expect(page.locator("[data-sonner-toast]")).toHaveCount(0, { timeout: 15_000 });
  backend.changeServerSettings("long.example.com");
  // An unchanged save is no change at all: something must differ for the version to matter.
  await main.getByLabel("Conflict text 1", { exact: true }).fill("You logged in from elsewhere");
  await toTop();
  await save.click();
  const changed = main.getByText(/^These settings changed since you opened them/);
  await expect(changed).toBeVisible();
  expect(await where(changed)).toEqual({ inColumn: true, underBar: false });
  await frameHolds(page, "a server's settings stacked, changed");
});

test("Delete is set apart by a rule, as the mode editor's (U121)", async ({ page, context }) => {
  await page.setViewportSize({ width: 1280, height: 720 });
  await atLongSettings(page, context);

  const apart = await page
    .getByRole("button", { name: "Delete", exact: true })
    .evaluate((button) => {
      const holder = button.closest("[data-slot=editor-apart] > *");
      const style = holder === null ? null : getComputedStyle(holder);
      return { border: style?.borderTopWidth, padding: style?.paddingTop };
    });

  expect(apart).toEqual({ border: "1px", padding: "12px" });
});
