// The screenshot baselines' own proofs (Plan.md P11.11; ADR-0020, U97, S131), in Playwright's pinned
// image only: the page's clock, zone, language and pixel ratio are fixed; a shot leaves out what
// happened before it, a tooltip however it opened and a focus kept on purpose; and it's refused
// while something would make it depend on more than its state.
import { writeFileSync } from "node:fs";
import process from "node:process";

import type { Page } from "@playwright/test";

import { type FakeBackend, OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { NOW, refusal } from "../shots";
import { problems, ready, type ShotOptions } from "./shoot";
import { enterCode, expectHome, logIn } from "./steps";

test.skip(refusal(process.platform, process.env) !== undefined, "only in the pinned image");

/** Signed in as the Owner, on Home. */
async function signIn(page: Page, backend: FakeBackend): Promise<void> {
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
}

/** That `shots` are one image, each saved beside the results when they aren't, to be seen. */
function same(shots: Readonly<Record<string, Buffer>>): void {
  const [first, ...rest] = Object.values(shots);
  if (rest.some((shot) => first === undefined || !shot.equals(first))) {
    for (const [name, shot] of Object.entries(shots)) {
      writeFileSync(test.info().outputPath(`${name}.png`), shot);
    }
  }
  expect(rest.every((shot) => first !== undefined && shot.equals(first))).toBe(true);
}

/** What a shot would take now, readied as `options` asks, then the page as it was. */
async function still(page: Page, options: ShotOptions = {}): Promise<Buffer> {
  const restore = await ready(page, options);
  try {
    expect(await problems(page, options)).toEqual([]);
    return await page.screenshot({ animations: "disabled", caret: "hide", scale: "css" });
  } finally {
    await restore();
  }
}

test("the page reads the fixed time, in UTC and en-US, at one device pixel", async ({
  page,
  context,
}) => {
  await fakeBackend(context);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();

  const read = await page.evaluate(() => ({
    now: new Date().toISOString(),
    zone: Intl.DateTimeFormat().resolvedOptions().timeZone,
    language: navigator.language,
    ratio: devicePixelRatio,
  }));

  expect(read).toEqual({ now: NOW, zone: "UTC", language: "en-US", ratio: 1 });
});

test("a shot of the rail leaves out a tooltip, however it opened", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await page.setViewportSize({ width: 900, height: 700 });
  await signIn(page, backend);
  const untouched = await still(page);

  await page
    .getByRole("group", { name: "Account" })
    .getByRole("button", { name: "Log out" })
    .hover();
  await expect(page.locator("[data-slot=tooltip]")).toBeVisible();
  const hovered = await still(page);
  await page.keyboard.press("Tab");
  await expect(page.locator("[data-slot=tooltip]")).toBeVisible();
  const tabbed = await still(page);

  same({ untouched, hovered, tabbed });
});

test("a focus kept on purpose stays, its outline and caret hidden for the shot, then back", async ({
  page,
  context,
}) => {
  await fakeBackend(context);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
  const field = page.getByLabel("Username");
  await field.focus();
  const drawn = () =>
    field.evaluate((element) => {
      const style = getComputedStyle(element);
      return {
        focused: document.activeElement === element,
        outline: style.outlineStyle,
        caret: style.caretColor,
      };
    });

  const restore = await ready(page, { keepFocus: true });
  const during = await drawn();
  await restore();

  expect(during).toEqual({ focused: true, outline: "none", caret: "rgba(0, 0, 0, 0)" });
  expect(await drawn()).toMatchObject({ focused: true, outline: "solid" });
  expect((await drawn()).caret).not.toBe("rgba(0, 0, 0, 0)");
});

test("a shot is refused while a tooltip stays open", async ({ page, context }) => {
  const backend = await fakeBackend(context);
  await page.setViewportSize({ width: 900, height: 700 });
  await signIn(page, backend);

  await page
    .getByRole("group", { name: "Account" })
    .getByRole("button", { name: "Log out" })
    .hover();
  await expect(page.locator("[data-slot=tooltip]")).toBeVisible();

  expect(await problems(page, { keepPointer: true })).toContain("a tooltip is open");
});

test("a shot is refused with the focus kept in the code boxes", async ({ page, context }) => {
  await fakeBackend(context);
  await page.goto("/");
  await logIn(page, OWNER);
  await expect(page.getByLabel("Code")).toBeFocused();

  expect(await problems(page, { keepFocus: true })).toContain("the code boxes have the focus");
});

test("a shot is refused while the column scrolls, naming the window it needs", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  await signIn(page, backend);

  await page.evaluate(() => {
    const tall = document.createElement("div");
    tall.style.height = "1000px";
    document.querySelector("main")?.append(tall);
  });

  expect(await problems(page, {})).toContainEqual(
    expect.stringMatching(/^the content scrolls: it needs a window \d+00 px tall$/),
  );
});
