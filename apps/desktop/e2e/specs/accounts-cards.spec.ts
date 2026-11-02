// The Accounts list in the real engine (Plan.md P11.11; ADR-0020, U106, U108, R209), as jsdom can't say
// what a click lands on: the whole card opens the account, while Link again and Approve never do.

import type { Page } from "@playwright/test";

import { type FakeBackend, MEMBER, OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";

/** `username` on the Accounts screen, the fake set up by `prepare`. */
async function onAccounts(
  page: Page,
  context: Parameters<typeof fakeBackend>[0],
  username: string,
  prepare: (backend: FakeBackend) => void = () => {},
) {
  const backend = await fakeBackend(context);
  prepare(backend);
  await page.goto("/");
  await logIn(page, username);
  await enterCode(page, backend.code(username));
  await expectHome(page, username);
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: /^Accounts/ })
    .click();
  await expect(
    page.getByRole("main").getByRole("heading", { name: "Accounts", level: 1 }),
  ).toBeVisible();
  return backend;
}

/** Clicks the card's "Type" fact with the raw mouse: the stretched link is what's on top there. */
async function clickFact(page: Page, name: string) {
  const box = await page
    .getByRole("listitem")
    .filter({ hasText: name })
    .getByText("Type", { exact: true })
    .boundingBox();
  if (box === null) {
    throw new Error(`no facts on ${name}`);
  }
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
}

test("a click on a fact opens the account", async ({ page, context }) => {
  await onAccounts(page, context, OWNER, (backend) => {
    backend.addAccount(OWNER);
  });

  await clickFact(page, "AfkBot1");

  await expect(page.getByRole("heading", { name: "AfkBot1", level: 1 })).toBeVisible();
});

test("Link again leads to its own screen, never the account's", async ({ page, context }) => {
  await onAccounts(page, context, OWNER, (backend) => {
    backend.addAccount(OWNER, "reauth_required");
  });
  const main = page.getByRole("main");
  await expect(main.getByRole("link", { name: "AfkBot1", exact: true })).toBeVisible();

  await main.getByRole("link", { name: "Link again" }).click();

  await expect(page.getByRole("heading", { name: "Link AfkBot1 again" })).toBeVisible();
});

test("the re-auth prompt's text stays on the list, and can be selected", async ({
  page,
  context,
}) => {
  await onAccounts(page, context, OWNER, (backend) => {
    backend.addAccount(OWNER, "reauth_required");
  });
  const prompt = page.getByRole("main").getByText(/no longer accepts/);

  await prompt.click();

  await expect(
    page.getByRole("main").getByRole("heading", { name: "Accounts", level: 1 }),
  ).toBeVisible();
});

test("Approve asks, there on the card, without opening the account", async ({ page, context }) => {
  await onAccounts(page, context, OWNER, (backend) => {
    backend.addAccount(MEMBER, "pending");
  });
  const main = page.getByRole("main");

  await main.getByRole("button", { name: "Approve AfkBot1" }).click();

  await expect(main.getByText(/Approve AfkBot1, linked by/)).toBeVisible();
  await expect(main.getByRole("heading", { name: "Accounts", level: 1 })).toBeVisible();
});

test("the tiles stand side by side, and stack at the minimum window", async ({ page, context }) => {
  await onAccounts(page, context, MEMBER);
  const tiles = page.locator("[data-slot=choice-tile]");
  await expect(tiles).toHaveCount(2);

  await page.setViewportSize({ width: 1200, height: 800 });
  await expect
    .poll(async () => (await tiles.nth(1).boundingBox())?.x)
    .toBeGreaterThan((await tiles.nth(0).boundingBox())?.x ?? 0);

  await page.setViewportSize({ width: 800, height: 600 });
  await expect
    .poll(async () => (await tiles.nth(1).boundingBox())?.y)
    .toBeGreaterThan((await tiles.nth(0).boundingBox())?.y ?? 0);
});

test("a tile's whole box takes the pointer, and the main one starts with the accent", async ({
  page,
  context,
}) => {
  await onAccounts(page, context, MEMBER);
  const main = page.getByRole("main");
  const microsoft = main.getByRole("link", { name: "Microsoft account" });
  const offline = main.getByRole("link", { name: "Offline account" });

  const border = (tile: typeof microsoft) =>
    tile.evaluate((one) => getComputedStyle(one).borderTopColor);
  const primary = await page.evaluate(() => {
    const probe = document.createElement("div");
    probe.style.color = "var(--primary)";
    document.body.append(probe);
    const read = getComputedStyle(probe).color;
    probe.remove();
    return read;
  });
  expect(await border(microsoft)).toBe(primary);
  expect(await border(offline)).not.toBe(primary);

  await offline.hover();
  await expect.poll(() => border(offline)).toBe(primary);

  const box = await offline.boundingBox();
  await page.mouse.click((box?.x ?? 0) + 3, (box?.y ?? 0) + 3);
  await expect(page.getByRole("heading", { name: "Add an offline account" })).toBeVisible();
});

test("the offline screen doesn't reopen on Back once it's done", async ({ page, context }) => {
  await onAccounts(page, context, MEMBER);
  const main = page.getByRole("main");
  await main.getByRole("link", { name: "Offline account" }).click();
  await main.getByLabel("Minecraft name").fill("AfkBot9");
  await main.getByRole("button", { name: "Add" }).click();
  await expect(main.getByRole("heading", { name: "Accounts", level: 1 })).toBeVisible();

  await page.getByRole("button", { name: "Back", exact: true }).click();

  await expect(page.getByRole("heading", { name: "Bots", level: 1 })).toBeVisible();
});
