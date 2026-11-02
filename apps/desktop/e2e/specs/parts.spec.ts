// What the shared parts' classes do in the real engine (Plan.md P11.11; ADR-0020, S112), where a
// Vitest has no stylesheet: a table's column drops by the width of its own box, and a spanning row
// doesn't widen its columns. Both read the classes as `components/ui/table.tsx` writes them, which
// Tailwind only generates because that file holds them.

import { OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn, push } from "./steps";
import { inScheme } from "./theme";

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
    // The real container's border, as components/ui/table.tsx draws it.
    box.className = "@container/table border";
    box.style.boxSizing = "border-box";
    const table = document.createElement("table");
    const row = table.insertRow();
    row.insertCell().textContent = "agent-main";
    const dropped = row.insertCell();
    dropped.id = "planted-dropped";
    dropped.className = "@max-[50.5rem]/table:hidden";
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
  await setWidth("810px");
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

test("a toast's focus shows the one outline, none of sonner's shadows", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await push(page, {
    update: "event",
    event: {
      type: "bot_died",
      bot_id: backend.bot("AfkBot1").id,
      at: new Date().toISOString(),
    },
  });
  const toast = page.locator("[data-sonner-toast]");
  const action = toast.getByRole("button", { name: "Open" });
  await expect(action).toBeVisible();
  await toast.hover();

  for (const [name, stop] of [
    ["the toast", toast],
    ["its close button", toast.locator("[data-close-button]")],
    ["its action", action],
  ] as const) {
    // A real Tab: a script's focus isn't always `:focus-visible`.
    let reached = false;
    for (let tabs = 0; !reached && tabs < 40; tabs += 1) {
      await page.keyboard.press("Tab");
      reached = await stop.evaluate((element) => element === document.activeElement);
    }
    expect(reached, name).toBe(true);
    const read = await stop.evaluate((element) => {
      for (const animation of document.getAnimations()) {
        if (animation.effect?.getComputedTiming().endTime !== Number.POSITIVE_INFINITY) {
          animation.finish();
        }
      }
      const style = getComputedStyle(element);
      return {
        focusVisible: element.matches(":focus-visible"),
        outline: style.outlineStyle,
        shadow: style.boxShadow,
      };
    });

    expect(read, name).toEqual({ focusVisible: true, outline: "solid", shadow: "none" });
  }
});

test("an error toast and a warning toast each hold their tint over the popover, in both themes", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  const id = backend.bot("AfkBot1").id;
  const at = new Date().toISOString();
  await push(page, { update: "event", event: { type: "bot_died", bot_id: id, at } });
  await push(page, {
    update: "event",
    event: { type: "bot_alert", bot_id: id, at, state: "failed", reason: "crash_loop" },
  });

  for (const [type, token] of [
    ["warning", "--warning"],
    ["error", "--destructive"],
  ] as const) {
    const toast = page.locator(`[data-sonner-toast][data-type=${type}]`);
    await expect(toast).toBeAttached();
    for (const scheme of ["dark", "light"] as const) {
      await inScheme(page, scheme);
      const read = await toast.evaluate((element, tint) => {
        const probe = document.createElement("div");
        probe.style.background = `color-mix(in srgb, var(${tint}) 12%, var(--popover))`;
        const plain = document.createElement("div");
        plain.style.background = "var(--popover)";
        document.body.append(probe, plain);
        const seen = {
          toast: getComputedStyle(element).backgroundColor,
          tinted: getComputedStyle(probe).backgroundColor,
          plain: getComputedStyle(plain).backgroundColor,
        };
        probe.remove();
        plain.remove();
        return seen;
      }, token);

      expect(read.toast, `${type} in ${scheme}`).toBe(read.tinted);
      expect(read.toast, `${type} in ${scheme}`).not.toBe(read.plain);
      expect(read.toast, `${type} in ${scheme}`).not.toBe("rgba(0, 0, 0, 0)");
    }
  }
});
