// An account's screen in the real engine (Plan.md P11.11; ADR-0020, U83, U105, U109, S112), as jsdom
// has no layout: two columns for a Manage holder, stacking as a bot's screen does; one column below
// Manage, never an empty right half; a table that drops columns rather than cut text; Delete set apart.

import type { Page } from "@playwright/test";

import { MEMBER, NEW_USER, OWNER } from "../fake/backend";
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
