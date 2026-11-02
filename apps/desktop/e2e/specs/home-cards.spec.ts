// Home's cards in the real engine (Plan.md P11.11; ADR-0020, U88, U91, S124), as jsdom has no layout
// and so can't say what a click lands on: Start, Stop and a refusal sit above the card's stretched
// link, and the rest of the card opens the bot.

import type { Page } from "@playwright/test";

import { type FakeBackend, OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";

/** The Owner on Home, with `bots` accounts, set up further by `prepare`. */
async function onHome(
  page: Page,
  context: Parameters<typeof fakeBackend>[0],
  bots = 1,
  prepare: (backend: FakeBackend) => void = () => {},
) {
  const backend = await fakeBackend(context);
  for (let added = 0; added < bots; added += 1) {
    backend.addAccount(OWNER);
  }
  prepare(backend);
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await expect(page.getByRole("link", { name: "Open AfkBot1" })).toBeVisible();
  return backend;
}

/** Where a fact's label on bot `name`'s card is: under the card's stretched link. */
async function factAt(page: Page, name: string, term: string) {
  const box = await page
    .getByRole("listitem")
    .filter({ hasText: name })
    .getByText(term, { exact: true })
    .boundingBox();
  if (box === null) {
    throw new Error(`no ${term} on ${name}`);
  }
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

/** How many columns the cards sit in. */
const columns = (page: Page) =>
  page.getByRole("main").evaluate((main) => {
    const lefts = [...main.querySelectorAll("li")].map((one) => one.getBoundingClientRect().left);
    return new Set(lefts).size;
  });

test("three columns at 1200x800, two at the minimum window", async ({ page, context }) => {
  await onHome(page, context, 6);
  await page.setViewportSize({ width: 1200, height: 800 });
  await expect.poll(() => columns(page)).toBe(3);

  await page.setViewportSize({ width: 800, height: 600 });
  await expect.poll(() => columns(page)).toBe(2);
});

test("Start stays on Home and starts the bot, and a fact opens it", async ({ page, context }) => {
  const backend = await onHome(page, context);

  await page.getByRole("button", { name: "Start AfkBot1" }).click();

  await expect(page.getByRole("heading", { name: "Bots", level: 1 })).toBeVisible();
  expect(backend.timesCalled("start_bot")).toBe(1);

  // The raw mouse: `click()` would refuse, as the stretched link is what's on top there.
  const mode = await factAt(page, "AfkBot1", "Mode");
  await page.mouse.click(mode.x, mode.y);

  await expect(page.getByRole("heading", { name: "AfkBot1", level: 1 })).toBeVisible();
});

test("Stop stays on Home", async ({ page, context }) => {
  const backend = await onHome(page, context, 1, (fake) => {
    fake.setBot("AfkBot1", {
      desiredState: "running",
      status: { kind: "reported", state: "online" },
    });
  });

  await page.getByRole("button", { name: "Stop AfkBot1" }).click();

  await expect(page.getByRole("heading", { name: "Bots", level: 1 })).toBeVisible();
  expect(backend.timesCalled("stop_bot")).toBe(1);
});

test("a click on a refusal's text stays on Home, and it can be selected", async ({
  page,
  context,
}) => {
  const backend = await onHome(page, context);
  backend.refuseNext("start_bot", 409, "conflict");

  await page.getByRole("button", { name: "Start AfkBot1" }).click();
  const alert = page.locator("[data-slot=card]").getByRole("alert");
  await expect(alert).toBeVisible();

  await alert.click();

  await expect(page.getByRole("heading", { name: "Bots", level: 1 })).toBeVisible();
  const hit = await alert.evaluate((held) => {
    const box = held.getBoundingClientRect();
    const top = document.elementFromPoint(box.left + 2, box.top + box.height / 2);
    return top !== null && held.contains(top);
  });
  expect(hit).toBe(true);
});

test("a card's name is in the mono font, and hovering marks the whole card", async ({
  page,
  context,
}) => {
  await onHome(page, context);
  const link = page.getByRole("link", { name: "Open AfkBot1" });
  await expect(link).toHaveCSS("font-family", /Plex Mono/);

  const card = page.locator("[data-slot=card]").filter({ hasText: "AfkBot1" });
  const before = await card.evaluate((one) => getComputedStyle(one).borderTopColor);
  const mode = await factAt(page, "AfkBot1", "Mode");
  await page.mouse.move(mode.x, mode.y);

  await expect
    .poll(() => card.evaluate((one) => getComputedStyle(one).borderTopColor))
    .not.toBe(before);
});
