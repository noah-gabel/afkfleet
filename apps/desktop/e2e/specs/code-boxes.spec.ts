// The six boxes in the real engine (Plan.md P11.11; ADR-0020, U86, S100), in Edge, under the release
// CSP: real keys and a real paste, what jsdom only pretends, and whether the digits stay on their
// cells. The user's hands-on adds a real authenticator, Narrator and Windows' scaling.

import type { Page } from "@playwright/test";

import { OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { expectHome, logIn } from "./steps";

/** The Owner at the code step: the boxes, with the keyboard already in them. */
async function atCode(page: Page, context: Parameters<typeof fakeBackend>[0]) {
  const backend = await fakeBackend(context);
  await page.goto("/");
  await logIn(page, OWNER);
  const boxes = page.getByLabel("Code");
  await expect(boxes).toBeFocused();
  return { backend, boxes };
}

/** Pastes `text` into what has the focus: a real Ctrl+V, from the real clipboard. */
async function paste(page: Page, text: string): Promise<void> {
  await page.evaluate((copied) => navigator.clipboard.writeText(copied), text);
  await page.keyboard.press("Control+V");
}

test("typing fills them, sends at the sixth, and logs in", async ({ page, context }) => {
  const { backend } = await atCode(page, context);

  await page.keyboard.type(backend.code(OWNER));

  await expectHome(page, OWNER, "Owner");
});

test("Backspace and the arrows are the input's own", async ({ page, context }) => {
  const { boxes } = await atCode(page, context);

  await page.keyboard.type("123");
  await expect(boxes).toHaveValue("123");
  await page.keyboard.press("Backspace");
  await expect(boxes).toHaveValue("12");

  await page.keyboard.press("ArrowLeft");
  await page.keyboard.type("9");
  await expect(boxes).toHaveValue("192");
  await page.keyboard.press("Home");
  await page.keyboard.press("Delete");
  await expect(boxes).toHaveValue("92");
});

test("a real paste fills them and logs in", async ({ page, context }) => {
  const { backend } = await atCode(page, context);
  const code = backend.code(OWNER);

  await paste(page, `${code.slice(0, 3)} ${code.slice(3)}`);

  await expectHome(page, OWNER);
});

test("a pasted recovery code swaps to the field, filled, and sends nothing", async ({
  page,
  context,
}) => {
  const { backend } = await atCode(page, context);
  const before = backend.code(OWNER);

  await paste(page, "abcd-efgh-ijkl-mnop-qrst");

  const recovery = page.getByLabel("Recovery code");
  await expect(recovery).toHaveValue("abcd-efgh-ijkl-mnop-qrst");
  await expect(recovery).toBeFocused();
  await expect(page.getByText("3 tries left.")).toBeVisible();
  expect(backend.code(OWNER)).toBe(before);
});

test("the digits sit on their cells, and six don't scroll the field", async ({ page, context }) => {
  const { backend, boxes } = await atCode(page, context);
  await page.evaluate(() => document.fonts.ready);
  // The answer waits, so all six stay in the boxes.
  backend.hold("login_mfa");

  await page.keyboard.type("123456");
  await expect(boxes).toHaveValue("123456");

  const placed = await boxes.evaluate((field: HTMLInputElement) => {
    const style = getComputedStyle(field);
    const canvas = document.createElement("canvas").getContext("2d");
    if (canvas === null) {
      throw new Error("no 2D context");
    }
    canvas.font = `${style.fontSize} ${style.fontFamily}`;
    const glyph = canvas.measureText("0").width;
    const advance = glyph + Number.parseFloat(style.letterSpacing);
    const left = field.getBoundingClientRect().left + Number.parseFloat(style.paddingLeft);
    const cells = [...document.querySelectorAll("[data-slot=code-cell]")].map((cell) =>
      cell.getBoundingClientRect(),
    );
    return {
      scrolled: field.scrollLeft,
      overflow: field.scrollWidth - field.clientWidth,
      off: cells.map((cell, index) =>
        Math.abs(left + index * advance + glyph / 2 - (cell.left + cell.width / 2)),
      ),
    };
  });

  expect(placed.scrolled).toBe(0);
  expect(placed.overflow).toBeLessThanOrEqual(0);
  expect(placed.off).toHaveLength(6);
  for (const off of placed.off) {
    expect(off).toBeLessThan(1);
  }
  backend.release();
});

test("the link swaps to a recovery code and back", async ({ page, context }) => {
  const { boxes } = await atCode(page, context);

  await page.getByRole("button", { name: "Use a recovery code" }).click();
  const recovery = page.getByLabel("Recovery code");
  await expect(recovery).toBeFocused();

  await page.getByRole("button", { name: "Use the 6-digit code" }).click();
  await expect(boxes).toBeFocused();
  await expect(boxes).toHaveValue("");
});

test("a wrong code clears them, keeps the focus and counts the try", async ({ page, context }) => {
  const { boxes } = await atCode(page, context);

  await page.keyboard.type("000000");

  await expect(page.getByText("Wrong code. 2 tries left.")).toBeVisible();
  await expect(boxes).toHaveValue("");
  await expect(boxes).toBeFocused();
});
