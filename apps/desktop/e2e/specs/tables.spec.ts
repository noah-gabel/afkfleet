// The admin tables as the window narrows and widens (Plan.md P11.11; ADR-0020, U122, R228), with the
// longest rows they can hold: at every width from 800 to 1800 px, 1 px at a time across the sidebar's
// jump at 1056 px, nothing scrolls sideways and no cell cuts its text, and the shown columns only
// ever grow as the window widens and shrink as it narrows, so dragging the window never shows a
// column, hides it and shows it again. With a short list and with one long enough for the content
// column's scrollbar, which this file shows, as WebView2 shows it.
import type { Page } from "@playwright/test";

import { type FakeBackend, MEMBER, NEW_USER, OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";

test.use({ launchOptions: { ignoreDefaultArgs: ["--hide-scrollbars"] } });
test.setTimeout(240_000);

/** `from` to `to`, `step` apart. */
function range(from: number, to: number, step: number): number[] {
  return Array.from({ length: Math.floor((to - from) / step) + 1 }, (_, n) => from + n * step);
}

/** Every width tried, widening: 1 px at a time across the sidebar's jump, 4 px elsewhere. */
const WIDTHS = [...range(800, 1036, 4), ...range(1040, 1072, 1), ...range(1076, 1800, 4)];

/** A username at the longest the server takes, 32 characters. */
function longName(n: number): string {
  return `longusername_with_32_characte${String(n).padStart(3, "0")}`;
}

/** What table `name` shows at `width`: its heads, what scrolls sideways, and any cut cell. */
async function at(page: Page, name: string, width: number) {
  await page.setViewportSize({ width, height: 800 });
  return page.evaluate(
    (label) =>
      new Promise<{ shown: string[]; sideways: string[]; cut: string[] }>((done) => {
        requestAnimationFrame(() =>
          requestAnimationFrame(() => {
            const table = document.querySelector(`table[aria-label="${label}"]`);
            const box = table?.closest("[data-slot=table-container]");
            const shown = [...(table?.querySelectorAll("thead th") ?? [])]
              .filter((head) => head.getBoundingClientRect().width > 0)
              .map((head) => head.textContent?.trim() || "(actions)");
            const sideways = [
              ["the box", box],
              ["the column", document.querySelector("[data-slot=content]")],
              ["the page", document.documentElement],
            ]
              .filter(([, element]) => {
                const held = element as Element | null | undefined;
                return held !== null && held !== undefined && held.scrollWidth > held.clientWidth;
              })
              .map(([where]) => String(where));
            const cut = [...(table?.querySelectorAll("th, td") ?? [])]
              .filter(
                (cell) =>
                  cell.getBoundingClientRect().width > 0 && cell.scrollWidth > cell.clientWidth,
              )
              .map((cell) => (cell.textContent ?? "").slice(0, 30));
            done({ shown, sideways, cut: cut.slice(0, 3) });
          }),
        );
      }),
    name,
  );
}

/**
 * Table `name` at every width, widening then narrowing: what scrolls sideways, what's cut, and a
 * column that went while widening or came while narrowing, but at one of `switches`, where a
 * screen goes from stacked to two columns by design and its table's box narrows. None, to pass.
 */
async function acrossWidths(
  page: Page,
  name: string,
  switches: readonly number[] = [],
): Promise<readonly string[]> {
  const problems: string[] = [];
  for (const widening of [true, false]) {
    let before: readonly string[] | null = null;
    let last = 0;
    for (const width of widening ? WIDTHS : [...WIDTHS].reverse()) {
      const now = await at(page, name, width);
      if (now.sideways.length > 0) {
        problems.push(`${width}: ${now.sideways.join(", ")} scrolls sideways`);
      }
      if (now.cut.length > 0) {
        problems.push(`${width}: cut ${now.cut.join(" | ")}`);
      }
      const then = before;
      const switched = switches.some((at) =>
        widening ? last < at && at <= width : width < at && at <= last,
      );
      const moved =
        then === null || switched
          ? []
          : widening
            ? then.filter((head) => !now.shown.includes(head))
            : now.shown.filter((head) => !then.includes(head));
      for (const head of moved) {
        problems.push(`${width}: ${head} ${widening ? "went, widening" : "came, narrowing"}`);
      }
      before = now.shown;
      last = width;
    }
  }
  return problems.slice(0, 10);
}

/** Signed in as the Owner, on the screen `link` opens in the nav. */
async function atScreen(page: Page, backend: FakeBackend, link: string): Promise<void> {
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: link }).click();
}

for (const [count, scrolls] of [
  [1, "with no scrollbar"],
  [30, "with the column's scrollbar"],
] as const) {
  test(`Users, ${scrolls}`, async ({ page, context }) => {
    const backend = await fakeBackend(context);
    for (let n = 1; n <= count; n += 1) {
      backend.addMember(longName(n), { disabled: n % 2 === 0 });
    }
    await atScreen(page, backend, "Users");
    await expect(page.getByRole("table", { name: "Users" })).toBeVisible();

    expect(await acrossWidths(page, "Users")).toEqual([]);
  });
}

test("the grants review's two tables, stacked and then side by side from 912 px (U124)", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  for (let n = 1; n <= 6; n += 1) {
    backend.share(backend.addAccount(OWNER), MEMBER, "control");
  }
  backend.share(backend.addAccount(MEMBER), NEW_USER, "view");
  await atScreen(page, backend, "Users");
  await page
    .getByRole("row", { name: new RegExp(MEMBER) })
    .getByRole("link", { name: /Review grants/ })
    .click();
  await page.getByRole("button", { name: /Show who has access/ }).click();
  const shared = page.getByRole("table", { name: /Who has access to/ });
  const name = (await shared.getAttribute("aria-label")) ?? "";

  expect(await acrossWidths(page, "Grants they hold", [912])).toEqual([]);
  expect(await acrossWidths(page, name, [912])).toEqual([]);
});

for (const [count, scrolls] of [
  [1, "with no scrollbar"],
  [30, "with the column's scrollbar"],
] as const) {
  test(`Invites, ${scrolls}`, async ({ page, context }) => {
    const backend = await fakeBackend(context);
    for (let n = 1; n <= count; n += 1) {
      const maker = longName(100 + n);
      backend.addAdmin(maker);
      backend.addMember(longName(n));
      backend.addInvite(maker, n % 2 === 0 ? longName(n) : undefined);
    }
    await atScreen(page, backend, "Invites");
    await expect(page.getByRole("table", { name: "Invites" })).toBeVisible();

    expect(await acrossWidths(page, "Invites")).toEqual([]);
  });
}

/** The agents a fleet could hold at their longest: names, versions, statuses and bots. */
function agents(backend: FakeBackend, count: number): void {
  const statuses = ["online", "offline", "expired", "disabled"] as const;
  for (let n = 1; n <= count; n += 1) {
    const status = statuses[n % statuses.length] ?? "online";
    backend.addAgent(`agent-on-the-host-with-a-long-name-${String(n).padStart(3, "0")}`, {
      status,
      enabled: status !== "disabled",
      bots: n % 3 === 0 ? 12 : 1,
      version: "0.11.0+0123456789abcdef",
    });
  }
}

for (const [count, scrolls] of [
  [2, "with no scrollbar"],
  [30, "with the column's scrollbar"],
] as const) {
  test(`Agents, ${scrolls}`, async ({ page, context }) => {
    const backend = await fakeBackend(context);
    agents(backend, count);
    await atScreen(page, backend, "Agents");
    await expect(page.getByRole("table", { name: "Agents" })).toBeVisible();

    expect(await acrossWidths(page, "Agents")).toEqual([]);
  });
}

test("a short agent name stays whole beside an expired agent's hint, at every width", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAgent("agent-main");
  backend.addAgent("agent-old", { enabled: false, status: "disabled", bots: 2 });
  backend.addAgent("agent-late", { status: "expired" });
  await atScreen(page, backend, "Agents");
  await expect(page.getByRole("table", { name: "Agents" })).toBeVisible();
  const broken: string[] = [];

  for (const width of range(800, 1800, 8)) {
    await page.setViewportSize({ width, height: 800 });
    const lines = await page.evaluate(
      () =>
        new Promise<{ name: string; lines: number }[]>((done) => {
          requestAnimationFrame(() =>
            requestAnimationFrame(() =>
              done(
                [...document.querySelectorAll("table[aria-label=Agents] tbody tr")].map((row) => {
                  // The name's text alone: a box around it would add a rect of its own.
                  const name = document
                    .createTreeWalker(row.querySelector("td") ?? row, NodeFilter.SHOW_TEXT)
                    .nextNode();
                  const text = document.createRange();
                  text.selectNodeContents(name ?? row);
                  const tops = [...text.getClientRects()].map((line) => Math.round(line.top));
                  return { name: text.toString(), lines: new Set(tops).size };
                }),
              ),
            ),
          );
        }),
    );
    broken.push(
      ...lines.filter((one) => one.lines > 1).map((one) => `${width}: ${one.name} on ${one.lines}`),
    );
  }

  expect(broken.slice(0, 6)).toEqual([]);
});
