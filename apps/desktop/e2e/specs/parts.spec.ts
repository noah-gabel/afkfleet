// What the shared parts' classes do in the real engine (Plan.md P11.11; ADR-0020, S112), where a
// Vitest has no stylesheet: a table's column drops by the width of its own box, and a spanning row
// doesn't widen its columns. Both read the classes as `components/ui/table.tsx` writes them, which
// Tailwind only generates because that file holds them.

import { expect, fakeBackend, test } from "../fixtures";

test("a column drops by its box's width, and a spanning row follows the table", async ({
  page,
  context,
}) => {
  await fakeBackend(context);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
  await page.evaluate(() => {
    const box = document.createElement("div");
    box.id = "planted";
    box.className = "@container/table";
    const table = document.createElement("table");
    const row = table.insertRow();
    row.insertCell().textContent = "agent-main";
    const dropped = row.insertCell();
    dropped.id = "planted-dropped";
    dropped.className = "@max-[50.625rem]/table:hidden";
    dropped.textContent = "1.0";
    const span = table.insertRow().insertCell();
    span.id = "planted-span";
    span.colSpan = 2;
    const inner = document.createElement("div");
    inner.className = "w-0 min-w-full";
    inner.textContent = "A question long enough to wrap, ".repeat(40);
    span.append(inner);
    box.append(table);
    document.body.append(box);
  });
  const display = () =>
    page.evaluate(
      () => getComputedStyle(document.getElementById("planted-dropped") as HTMLElement).display,
    );
  const setWidth = (width: string) =>
    page.evaluate((value) => {
      (document.getElementById("planted") as HTMLElement).style.width = value;
    }, width);

  await setWidth("809px");
  expect(await display()).toBe("none");
  await setWidth("811px");
  expect(await display()).toBe("table-cell");

  // The long text wraps inside the kept columns' width instead of pushing the table wider.
  await setWidth("400px");
  const widths = await page.evaluate(() => ({
    box: (document.getElementById("planted") as HTMLElement).getBoundingClientRect().width,
    scroll: (document.getElementById("planted") as HTMLElement).scrollWidth,
    span: (document.getElementById("planted-span") as HTMLElement).getBoundingClientRect().width,
  }));
  expect(widths.scroll).toBeLessThanOrEqual(Math.ceil(widths.box));
  expect(widths.span).toBeLessThanOrEqual(Math.ceil(widths.box));

  await page.evaluate(() => document.getElementById("planted")?.remove());
});
