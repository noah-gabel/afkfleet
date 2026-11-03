// An account's screen in the real engine (Plan.md P11.11; ADR-0020, U83, U105, U109, S112), as jsdom
// has no layout: two columns for a Manage holder, stacking as a bot's screen does; one column below
// Manage, never an empty right half; a table that drops columns rather than cut text; Delete set apart.

import type { Page } from "@playwright/test";

import { MEMBER, NEW_USER, OWNER, PASSWORD } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";

/** `username` on AfkBot1's screen, the Owner's account, shared with `shared`. */
async function atAccount(
  page: Page,
  context: Parameters<typeof fakeBackend>[0],
  username: string,
  shared: readonly (readonly [string, "view" | "control" | "manage"])[] = [],
) {
  const backend = await fakeBackend(context);
  const account = backend.addAccount(OWNER);
  for (const [user, level] of shared) {
    backend.share(account, user, level);
  }
  await page.goto("/");
  await logIn(page, username);
  await enterCode(page, backend.code(username));
  await expectHome(page, username);
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: /^Accounts/ })
    .click();
  await page.getByRole("main").getByRole("link", { name: "AfkBot1", exact: true }).click();
  await expect(page.getByRole("heading", { name: "AfkBot1", level: 1 })).toBeVisible();
  return backend;
}

const main = (page: Page) => page.getByRole("main");
const access = (page: Page) => main(page).getByRole("region", { name: "Who has access" });
const details = (page: Page) => main(page).getByRole("region", { name: "AfkBot1" });

/** Whether Who has access stands beside the account's box. */
async function sideBySide(page: Page): Promise<boolean> {
  const [left, right] = await Promise.all([
    details(page).boundingBox(),
    access(page).boundingBox(),
  ]);
  if (left === null || right === null) {
    throw new Error("no columns");
  }
  return right.x >= left.x + left.width;
}

for (const [width, beside] of [
  [800, false],
  [911, false],
  [912, true],
  [1056, true],
  [1200, true],
] as const) {
  test(`at ${width} px a Manage holder's screen is ${beside ? "two columns" : "stacked"}`, async ({
    page,
    context,
  }) => {
    await atAccount(page, context, OWNER, [[MEMBER, "view"]]);

    await page.setViewportSize({ width, height: 700 });

    await expect.poll(() => sideBySide(page)).toBe(beside);
  });
}

test("below Manage it's one column, with no empty right half", async ({ page, context }) => {
  await atAccount(page, context, MEMBER, [[MEMBER, "view"]]);
  await page.setViewportSize({ width: 1200, height: 800 });

  await expect(access(page)).toHaveCount(0);
  await expect(main(page).getByRole("button", { name: "Leave", exact: true })).toBeVisible();
  const columns = await main(page).evaluate(
    (held) =>
      getComputedStyle(held.querySelector("[data-slot=account-columns]") as Element)
        .gridTemplateColumns,
  );
  expect(columns.trim().split(/\s+/)).toHaveLength(1);
});

test("Delete is set apart from Link again, on its own line at the box's foot", async ({
  page,
  context,
}) => {
  await atAccount(page, context, OWNER);
  const again = details(page).getByRole("link", { name: "Link again" });
  const remove = details(page).getByRole("button", { name: "Delete", exact: true });

  const [top, foot] = await Promise.all([again.boundingBox(), remove.boundingBox()]);
  expect(foot?.y).toBeGreaterThan((top?.y ?? 0) + (top?.height ?? 0));
  const line = await remove.evaluate(
    (button) =>
      getComputedStyle(button.closest("[data-slot=account-foot]") as Element).borderTopWidth,
  );
  expect(line).toBe("1px");
});

test("the access table drops When first, then Given by, and never scrolls or cuts", async ({
  page,
  context,
}) => {
  await atAccount(page, context, OWNER, [
    [MEMBER, "control"],
    [NEW_USER, "view"],
  ]);
  const table = access(page).getByRole("table", { name: "Who has access" });
  const heads = () =>
    table.evaluate((held) =>
      [...held.querySelectorAll("th")]
        .filter((head) => getComputedStyle(head).display !== "none")
        .map((head) => head.textContent),
    );
  /** How many cells each body row shows: as many as there are heads. */
  const cells = () =>
    table.evaluate((held) =>
      [...held.querySelectorAll("tbody tr")].map(
        (row) =>
          [...row.querySelectorAll("td")].filter(
            (cell) => getComputedStyle(cell).display !== "none",
          ).length,
      ),
    );
  const fits = () =>
    table.evaluate((held) => {
      const box = held.parentElement as HTMLElement;
      const inside = [...held.querySelectorAll("td")].every(
        (cell) => cell.scrollWidth <= cell.clientWidth,
      );
      return box.scrollWidth <= box.clientWidth && inside;
    });

  for (const [width, shown] of [
    [1800, ["User", "Access", "Given by", "When", "Actions"]],
    [1200, ["User", "Access", "Given by", "Actions"]],
    [1056, ["User", "Access", "Actions"]],
    [912, ["User", "Access", "Actions"]],
    [800, ["User", "Access", "Given by", "When", "Actions"]],
  ] as const) {
    await page.setViewportSize({ width, height: 800 });
    await expect.poll(heads, `at ${width} px`).toEqual(shown);
    expect(await cells(), `at ${width} px`).toEqual([shown.length, shown.length]);
    expect(await fits(), `at ${width} px`).toBe(true);
  }
});

test("a row's question opens under it and closes with no empty row left", async ({
  page,
  context,
}) => {
  await atAccount(page, context, OWNER, [[MEMBER, "view"]]);
  const table = access(page).getByRole("table", { name: "Who has access" });
  await expect(table.getByRole("row")).toHaveCount(2);

  await table.getByRole("button", { name: `Revoke ${MEMBER}'s access` }).click();
  await expect(table.getByRole("row")).toHaveCount(3);

  await table.getByRole("button", { name: "Cancel" }).click();
  await expect(table.getByRole("row")).toHaveCount(2);
});

test("the owner's own row says You where a Manage grantee's does", async ({ page, context }) => {
  await atAccount(page, context, MEMBER, [[MEMBER, "manage"]]);

  const mine = access(page).getByRole("row", { name: new RegExp(MEMBER) });
  await expect(mine.getByText("You", { exact: true })).toBeVisible();
  await expect(mine.getByRole("button", { name: /Revoke/ })).toHaveCount(0);
});

test("Share with suggests a name by the keyboard, and sharing it still takes the password", async ({
  page,
  context,
}) => {
  await atAccount(page, context, OWNER);
  const field = main(page).getByRole("combobox", { name: "Username" });

  // All of it but the last letter, so it matches this name alone.
  await field.pressSequentially(MEMBER.slice(0, -1));
  const option = main(page).getByRole("option", { name: MEMBER });
  await expect(option).toBeVisible();
  await page.keyboard.press("ArrowDown");
  await expect(option).toHaveAttribute("aria-selected", "true");
  await expect(field).toBeFocused();
  await page.keyboard.press("Enter");

  await expect(field).toHaveValue(MEMBER);
  await expect(main(page).getByRole("listbox")).toHaveCount(0);
  await expect(main(page).getByLabel("Current password")).toHaveCount(0);

  await main(page).getByRole("button", { name: "Share" }).click();
  await main(page).getByLabel("Current password").fill(PASSWORD);
  await main(page).getByRole("button", { name: "Continue" }).click();
  await expect(access(page).getByRole("table", { name: "Who has access" })).toContainText(MEMBER);
});

test("Share with takes a click on a name, which keeps the focus and sends nothing", async ({
  page,
  context,
}) => {
  const backend = await atAccount(page, context, OWNER);
  const field = main(page).getByRole("combobox", { name: "Username" });

  await field.pressSequentially(MEMBER.slice(0, 2));
  await main(page).getByRole("option", { name: MEMBER }).click();

  await expect(field).toHaveValue(MEMBER);
  await expect(field).toBeFocused();
  expect(backend.timesCalled("create_grant")).toBe(0);
});

test("Share with still takes an exact name when the suggestions are limited", async ({
  page,
  context,
}) => {
  const backend = await atAccount(page, context, OWNER);
  backend.refuseNext("grant_candidates", 429, "rate_limited", 60);
  const field = main(page).getByRole("combobox", { name: "Username" });

  await field.pressSequentially(MEMBER);
  await expect.poll(() => backend.timesCalled("grant_candidates")).toBeGreaterThan(0);

  await expect(main(page).getByRole("alert")).toHaveCount(0);
  await main(page).getByRole("button", { name: "Share" }).click();
  await expect(main(page).getByLabel("Current password")).toBeVisible();
});
