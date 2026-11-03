// A server's settings in the real engine (Plan.md P11.11; ADR-0020, U121, R232): the mode editor's
// columns, the texts and the formats scrolling on their own beside the address with Save in view,
// and stacked, Save and Cancel in a bar at the column's foot. The scrollbars show, as WebView2
// shows them.
import type { Page } from "@playwright/test";

import { OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";
import { frameHolds } from "./theme";

test.use({ launchOptions: { ignoreDefaultArgs: ["--hide-scrollbars"] } });

/** The Owner on a server's settings holding sixteen texts and eight formats. */
async function atLongSettings(page: Page, context: Parameters<typeof fakeBackend>[0]) {
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
