// The script editor as one story (Plan.md P15.24, P15.25, P15.35; ADR-0036, D15, D16; ADR-0045),
// in Edge under the release CSP: CodeMirror in its shadow root with its styles there and none in
// the document (the fixtures' guard), its highlighting in the app's tokens, completion after a dot
// and for a hook, hover help, the marks from the check and from the description, Tab indenting and
// Esc then Tab leaving, a starter template, the any-time warning, the save; and a Member's
// read-only view of a shared script. The real compiler's check is the desktop crate's; the fake
// stands in for it (its `kees`).
import type { Locator, Page } from "@playwright/test";

import { MEMBER, OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn } from "./steps";

/** The editor's content, inside its host's shadow root (Playwright's locators pierce it). */
function content(page: Page): Locator {
  return page.locator('[data-slot="script-editor"] .cm-content');
}

/** The computed colour of the token `text` in the editor, and of the token `name` on the root. */
async function colours(page: Page, text: string, name: string) {
  return page.evaluate(
    ([word, token]) => {
      const root = document.querySelector('[data-slot="script-editor"]')?.shadowRoot;
      const span = [...(root?.querySelectorAll(".cm-line span") ?? [])].find(
        (one) => one.textContent === word,
      );
      const probe = document.createElement("span");
      probe.style.color = `var(${token})`;
      document.body.append(probe);
      const expected = getComputedStyle(probe).color;
      probe.remove();
      return { actual: span === undefined ? null : getComputedStyle(span).color, expected };
    },
    [text, name] as const,
  );
}

test("the Owner writes a scripted mode in the editor and saves it", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  const main = page.getByRole("main");
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Modes" }).click();
  await main.getByRole("link", { name: "New mode" }).click();
  await main.getByLabel("Name").fill("Cobble");
  await main.getByLabel("What it runs").selectOption("script");

  // The example fills the script and the blocks it mines.
  await main.getByLabel("Start from an example").selectOption("cobblestone_generator");
  await main.getByRole("button", { name: "Use example" }).click();
  await expect(content(page)).toContainText("fn on_join(first)");
  await expect(main.getByLabel("Block 1", { exact: true })).toHaveValue("minecraft:cobblestone");

  // Highlighting in the app's tokens: a keyword in --primary, a comment in --muted-foreground.
  const keyword = await colours(page, "fn", "--primary");
  expect(keyword.actual).toBe(keyword.expected);

  // Completion after a dot, from the description.
  await content(page).click();
  await page.keyboard.press("Control+End");
  await page.keyboard.press("Enter");
  await page.keyboard.type("fn helper() {\nkeys.");
  const list = page.locator('[data-slot="script-editor"] .cm-tooltip-autocomplete');
  await expect(list).toBeVisible();
  await expect(list.getByRole("option", { name: /^forward/ })).toBeVisible();
  await page.keyboard.press("Escape");

  // A member the compiler can't see, and the check's own finding, marked and listed.
  await page.keyboard.type("forwrd.hold();\nkees.jump.tap();");
  await expect(main.getByRole("button", { name: /keys has no forwrd/ })).toBeVisible();
  await expect(main.getByRole("button", { name: /`kees` isn't defined$/ })).toBeVisible();
  await expect(page.locator('[data-slot="script-editor"] .cm-lintRange-error')).toHaveCount(2);

  // Hover help over a game object.
  await page
    .locator('[data-slot="script-editor"] .cm-line span', { hasText: /^keys$/ })
    .first()
    .hover();
  await expect(page.locator('[data-slot="script-editor"] .cm-tooltip-hover')).toContainText(
    "The keys of a vanilla keyboard.",
  );

  // Tab indents; Esc, then Tab, leaves the editor.
  await content(page).click();
  await page.keyboard.press("Control+End");
  const before = await content(page).textContent();
  await page.keyboard.press("Tab");
  expect(await content(page).textContent()).not.toBe(before);
  await page.keyboard.press("Escape");
  await page.keyboard.press("Tab");
  expect(
    await page.evaluate(() => document.activeElement?.getAttribute("data-slot") ?? null),
  ).not.toBe("script-editor");

  // Clean the text again, mark a pattern as at any time, and save.
  await content(page).click();
  await page.keyboard.press("Control+A");
  await page.keyboard.type("fn on_join(first) {\nkeys.jump.tap();\n}");
  await main.getByRole("button", { name: "Add pattern" }).click();
  const pattern = main.getByRole("group", { name: "Pattern 1" });
  await pattern.getByLabel("When it counts").selectOption("any_time");
  await expect(pattern.getByText(/^Riskier:/)).toBeVisible();
  await pattern.getByRole("button", { name: "Remove pattern 1" }).click();
  await main.getByRole("button", { name: "Save", exact: true }).click();
  await expect(main.getByRole("heading", { name: "Cobble" })).toBeVisible();
  await expect(content(page)).toContainText("keys.jump.tap();");
});

test("a Member sees a shared script read-only, with nothing to copy", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  backend.addScriptedMode(
    OWNER,
    "Shared farm",
    {
      source: "fn on_join(first) {\n    mouse.right.hold();\n}\n",
      leash: 8,
      tool_guard: 2,
      block_guard: [],
      messages: [],
      patterns: [],
    },
    "shared",
  );
  await page.goto("/");
  await logIn(page, MEMBER);
  await enterCode(page, backend.code(MEMBER));
  await expectHome(page, MEMBER);
  const main = page.getByRole("main");
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Modes" }).click();
  await main.getByRole("link", { name: "Open Shared farm" }).click();

  await expect(content(page)).toContainText("mouse.right.hold();");
  await content(page).click();
  await page.keyboard.type("typed");
  await expect(content(page)).not.toContainText("typed");
  await expect(main.getByRole("button", { name: "Make a copy" })).toHaveCount(0);
  await expect(main.getByLabel("What it runs")).toHaveCount(0);
});
