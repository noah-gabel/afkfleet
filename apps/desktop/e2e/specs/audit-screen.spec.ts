// The audit log in the real engine (Plan.md P11.11; ADR-0020, U122, S132), as jsdom has no layout:
// one table, which stacks each entry into a labelled block below a 784 px box. As the window widens
// the form only ever goes stacked to table, and back the other way, never in between; nothing
// scrolls sideways or is cut; Chromium's own accessibility tree reads one table with its seven
// heads and every entry's seven cells in both forms, no label among them; and a focused link keeps
// the focus through a resize. With the longest entries, an IPv6 address and an unreadable row; the
// scrollbars show, as WebView2 shows them.
import type { Page } from "@playwright/test";

import { type FakeBackend, OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";
import { frameHolds } from "./theme";

test.use({ launchOptions: { ignoreDefaultArgs: ["--hide-scrollbars"] } });
test.setTimeout(180_000);

/** `count` entries at their longest, then an unreadable one. */
function entries(backend: FakeBackend, count: number): void {
  for (let n = 1; n <= count; n += 1) {
    backend.addAuditEntry({
      action: n % 2 === 0 ? "agent.connect" : "security.agent_identity_conflict",
      outcome: n % 3 === 0 ? "failure" : "success",
      ip: "2001:db8:ffff:ffff:ffff:ffff:ffff:fffe",
      target: {
        kind: "agent",
        id: `01900000-0000-7000-8000-0000000003${String(n).padStart(2, "0")}`,
      },
      metadata: [
        { key: "reason", text: "name_mismatch" },
        { key: "agent_name", text: "agent-on-the-host-with-a-long-name-and-more-001" },
      ],
    });
  }
  backend.addUnreadableEntry();
}

/** The Owner on the audit log. */
async function atLog(page: Page, backend: FakeBackend): Promise<void> {
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Audit log" })
    .click();
  await expect(page.getByRole("table", { name: "Audit log" })).toBeVisible();
}

/** At `width`: whether the log is stacked, what scrolls sideways, and any cut cell. */
async function at(page: Page, width: number) {
  await page.setViewportSize({ width, height: 800 });
  return page.evaluate(
    () =>
      new Promise<{ stacked: boolean; sideways: boolean; cut: number }>((done) => {
        requestAnimationFrame(() =>
          requestAnimationFrame(() => {
            const table = document.querySelector("table[aria-label='Audit log']");
            const box = table?.closest("[data-slot=table-container]");
            const scrolls = (element: Element | null | undefined) =>
              element !== null &&
              element !== undefined &&
              element.scrollWidth > element.clientWidth;
            done({
              stacked:
                table !== null &&
                table !== undefined &&
                getComputedStyle(table).display === "block",
              sideways:
                scrolls(box) ||
                scrolls(document.querySelector("[data-slot=content]")) ||
                scrolls(document.documentElement),
              cut: [...(table?.querySelectorAll("td") ?? [])].filter(
                (cell) =>
                  cell.getBoundingClientRect().width > 0 && cell.scrollWidth > cell.clientWidth,
              ).length,
            });
          }),
        );
      }),
  );
}

/** From 800 to 1800 px and back: where the form goes the wrong way, scrolls or cuts. */
async function acrossWidths(page: Page): Promise<readonly string[]> {
  const widths = [
    ...Array.from({ length: 61 }, (_, n) => 800 + n * 4),
    ...Array.from({ length: 33 }, (_, n) => 1040 + n),
    ...Array.from({ length: 182 }, (_, n) => 1076 + n * 4),
  ];
  const problems: string[] = [];
  for (const widening of [true, false]) {
    let was: boolean | null = null;
    for (const width of widening ? widths : [...widths].reverse()) {
      const now = await at(page, width);
      if (now.sideways) {
        problems.push(`${width}: scrolls sideways`);
      }
      if (now.cut > 0) {
        problems.push(`${width}: ${now.cut} cells cut`);
      }
      if (was !== null && now.stacked !== was && now.stacked === widening) {
        problems.push(
          `${width}: ${now.stacked ? "stacked again, widening" : "a table again, narrowing"}`,
        );
      }
      was = now.stacked;
    }
  }
  return problems.slice(0, 10);
}

for (const [count, scrolls] of [
  [1, "with no scrollbar"],
  [40, "with the column's scrollbar"],
] as const) {
  test(`stacks below a 784 px box and is a table from there on, both ways, ${scrolls}`, async ({
    page,
    context,
  }) => {
    const backend = await fakeBackend(context);
    entries(backend, count);
    await atLog(page, backend);

    expect(await acrossWidths(page)).toEqual([]);
    expect((await at(page, 800)).stacked).toBe(true);
    expect((await at(page, 1056)).stacked).toBe(false);
  });
}

test("Chromium reads one table, seven heads and every entry's cells in both forms", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  entries(backend, 3);
  await atLog(page, backend);
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Accessibility.enable");

  for (const width of [800, 1280]) {
    await at(page, width);
    const { nodes } = await cdp.send("Accessibility.getFullAXTree");
    const role = (node: (typeof nodes)[number]) => String(node.role?.value ?? "");
    const named = (node: (typeof nodes)[number]) => String(node.name?.value ?? "");
    const tables = nodes.filter((node) => role(node) === "table" && named(node) === "Audit log");
    // Chromium names a head as drawn, uppercase; the words are what matter here.
    const heads = nodes
      .filter((node) => role(node) === "columnheader")
      .map((node) => named(node).toLowerCase());
    const rows = nodes.filter((node) => role(node) === "row");
    const cellsOf = (row: (typeof nodes)[number]) =>
      (row.childIds ?? []).filter((child) =>
        nodes.some((node) => node.nodeId === child && ["cell", "gridcell"].includes(role(node))),
      ).length;

    expect(tables, `at ${width}`).toHaveLength(1);
    expect(heads, `at ${width}`).toEqual([
      "when",
      "who",
      "what",
      "on",
      "result",
      "from",
      "details",
    ]);
    // The heads' row, three entries of seven cells and the unreadable one of one, in any order.
    expect(
      rows.map(cellsOf).sort((one, two) => one - two),
      `at ${width}`,
    ).toEqual([0, 1, 7, 7, 7]);
    expect(
      nodes.filter((node) => role(node) === "link" && named(node).startsWith("Only agent ")),
      `at ${width}`,
    ).toHaveLength(3);
  }
});

test("stacked, every entry's values start at one x, whichever labels it has", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  entries(backend, 2);
  backend.addAuditEntry({ action: "agent.connect", outcome: "failure" });
  await atLog(page, backend);
  await at(page, 800);

  const starts = await page.evaluate(() => [
    ...new Set(
      // A labelled value: the unreadable row has no label and spans the whole entry.
      [
        ...document.querySelectorAll(
          "table[aria-label='Audit log'] td:has(> [data-slot=cell-label]) > div",
        ),
      ].map((value) => Math.round(value.getBoundingClientRect().left)),
    ),
  ]);

  expect(starts).toHaveLength(1);
});

test("a focused link keeps the focus as the log stacks", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  entries(backend, 3);
  await page.setViewportSize({ width: 1280, height: 800 });
  await atLog(page, backend);
  const link = page.getByRole("link", { name: /^Only agent / }).first();
  await link.focus();

  await at(page, 800);

  await expect(link).toBeFocused();
  await frameHolds(page, "the audit log, stacked");
});
