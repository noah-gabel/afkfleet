// The screenshot baselines' one way to shoot (Plan.md P11.11; ADR-0020, U97, U125, S131): a sweep
// row in both themes, in Playwright's pinned image only. Before each shot the pointer rests where it
// touches no control, the sidebar or a toast, the focus is let go, every command the page sent has
// its answer (but a read the fake holds back for a loading state, and the live events watch), and
// the shot is refused while a tooltip, a focus indicator, a font still loading or a scrolled column
// would make it depend on where things happened to be. The row's own window is kept; a row asks
// for a taller one, the smallest one too, or its column's end as well.
import type { Page } from "@playwright/test";

import { expect, test, unanswered } from "../fixtures";
import { SHOT_ANNOTATION, SMALL, slug } from "../shots";
import { inScheme } from "./scheme";

/** What a row asks of its shots. */
export interface ShotOptions {
  /** The window's height for the shot, a multiple of 100, when its content needs more. */
  readonly height?: number;
  /** Also shot at the smallest window, 800x600, for a layout that changes with width. */
  readonly small?: boolean;
  /** The small shot's height, a multiple of 100, when its content needs more than 600. */
  readonly smallHeight?: number;
  /**
   * The column scrolls by design: shot at its top and at its end. `all` requires the two to cover
   * all of it (a stacked bot's screen); `ends` leaves out a long middle on purpose (a long mode).
   */
  readonly end?: "all" | "ends";
  /** How many toasts the row shows; the shot waits until exactly that many are up. */
  readonly toasts?: number;
  /** The pointer stays where the row put it: on its toast, or on a tile whose hover is the row. */
  readonly keepPointer?: boolean;
  /** The focus stays, as the row's state needs it (an open list); its outline is hidden meanwhile. */
  readonly keepFocus?: boolean;
  /** Not shot: a row that shows a tooltip, which the baselines leave out. */
  readonly skip?: boolean;
}

/** What may never be under the resting pointer. */
const BUSY =
  "a, button, input, select, textarea, label, summary, [role=button], [role=option], [role=combobox], aside, [data-sonner-toast], [data-slot=tooltip]";

/** Puts the pointer where it touches nothing a hover could change, or fails saying so. */
async function park(page: Page): Promise<void> {
  const point = await page.evaluate((busy) => {
    const { innerWidth: width, innerHeight: height } = window;
    for (const y of [4, Math.round(height / 2), height - 4, 24]) {
      for (const x of [width - 4, width - 24, Math.round(width * 0.75), Math.round(width / 2)]) {
        if (!document.elementsFromPoint(x, y).some((element) => element.matches(busy))) {
          return { x, y };
        }
      }
    }
    return null;
  }, BUSY);
  expect(point, "nowhere to rest the pointer that touches no control").not.toBeNull();
  await page.mouse.move(point?.x ?? 0, point?.y ?? 0);
}

/**
 * Lets the focus go, or for a row that keeps it, hides its outline and its caret through the CSSOM
 * (never a `<style>`, which the release CSP refuses, so Playwright's own caret hiding doesn't
 * reach the page). Answers how to put them back.
 */
async function letGo(page: Page, keepFocus: boolean): Promise<() => Promise<void>> {
  if (!keepFocus) {
    await page.evaluate(() => {
      const focused = document.activeElement;
      if (focused instanceof HTMLElement) {
        focused.blur();
      }
    });
    return async () => {};
  }
  await page.evaluate(() => {
    const focused = document.activeElement;
    if (focused instanceof HTMLElement) {
      focused.dataset.shotOutline = focused.style.getPropertyValue("outline-style");
      focused.dataset.shotCaret = focused.style.getPropertyValue("caret-color");
      focused.style.setProperty("outline-style", "none", "important");
      focused.style.setProperty("caret-color", "transparent", "important");
    }
  });
  return () =>
    page.evaluate(() => {
      const focused = document.querySelector<HTMLElement>("[data-shot-outline]");
      if (focused !== null) {
        for (const [property, held] of [
          ["outline-style", focused.dataset.shotOutline],
          ["caret-color", focused.dataset.shotCaret],
        ] as const) {
          focused.style.removeProperty(property);
          if (held !== undefined && held !== "") {
            focused.style.setProperty(property, held);
          }
        }
        delete focused.dataset.shotOutline;
        delete focused.dataset.shotCaret;
      }
    });
}

/** Whatever would make a shot depend on more than its state: none, for a shot to be taken. */
export function problems(page: Page, options: ShotOptions): Promise<readonly string[]> {
  return page.evaluate(
    ({ keepFocus, end }) => {
      const found: string[] = [];
      if (document.querySelector("[data-slot=tooltip]") !== null) {
        found.push("a tooltip is open");
      }
      const focused = document.activeElement;
      if (focused?.matches("[data-focus-indicator=cells]")) {
        found.push("the code boxes have the focus");
      } else if (!keepFocus && document.querySelector(":focus-visible") !== null) {
        found.push("something shows its focus");
      }
      if ([...document.fonts].some((face) => face.status === "loading")) {
        found.push("a font is still loading");
      }
      if (!end) {
        for (const slot of ["screen", "content"]) {
          const box = document.querySelector(`[data-slot=${slot}]`);
          if (box !== null && box.scrollHeight > box.clientHeight) {
            const needed =
              Math.ceil((innerHeight + box.scrollHeight - box.clientHeight) / 100) * 100;
            found.push(`the ${slot} scrolls: it needs a window ${needed} px tall`);
          }
        }
      }
      return found;
    },
    { keepFocus: options.keepFocus === true, end: options.end !== undefined },
  );
}

/**
 * Waits until every command the page sent has its answer (`unanswered`), exactly `count` toasts
 * are up, none on its way out, and every font has loaded.
 */
async function settled(page: Page, count: number): Promise<void> {
  await expect
    .poll(() => unanswered(page), { message: "commands still unanswered", timeout: 10_000 })
    .toEqual([]);
  await expect(page.locator("[data-sonner-toast]:not([data-removed=true])")).toHaveCount(count, {
    timeout: 10_000,
  });
  await page.evaluate(() => document.fonts.ready.then(() => undefined));
}

/**
 * Readies the page for a shot as `options` asks: its toasts settled, the pointer at rest and the
 * focus let go. Answers how to put a kept focus's outline back.
 */
export async function ready(page: Page, options: ShotOptions): Promise<() => Promise<void>> {
  await settled(page, options.toasts ?? 0);
  if (options.keepPointer !== true) {
    await park(page);
  }
  return letGo(page, options.keepFocus === true);
}

/** The row's shots in both themes, as `name-light.png` and `name-dark.png`. */
async function inBothThemes(page: Page, name: string, options: ShotOptions): Promise<void> {
  for (const scheme of ["dark", "light"] as const) {
    await inScheme(page, scheme);
    expect(await problems(page, options), `${name} in ${scheme}`).toEqual([]);
    await expect(page).toHaveScreenshot(`${name}-${scheme}.png`);
    // What the run shot, for the orphans' check (e2e/orphans-reporter.ts).
    test.info().annotations.push({ type: SHOT_ANNOTATION, description: `${name}-${scheme}.png` });
  }
}

/** Waits two frames, so a new window size is laid out. */
function laidOut(page: Page): Promise<void> {
  return page.evaluate(
    () =>
      new Promise<void>((done) => requestAnimationFrame(() => requestAnimationFrame(() => done()))),
  );
}

/** Shoots `row` as `options` asks: its baselines in the screenshots' project (U125). */
export async function shoot(page: Page, row: string, options: ShotOptions = {}): Promise<void> {
  if (options.skip === true) {
    return;
  }
  const name = slug(row);
  const before = page.viewportSize();
  expect(before, "the page has a window size").not.toBeNull();
  const width = before?.width ?? 0;
  const height = before?.height ?? 0;
  if (options.height !== undefined) {
    await page.setViewportSize({ width, height: options.height });
    await laidOut(page);
  }
  const restore = await ready(page, options);
  try {
    await inBothThemes(page, name, options);
    if (options.end !== undefined) {
      const span = await page.evaluate(() => {
        const column = document.querySelector("[data-slot=content]");
        if (column === null) {
          return null;
        }
        const tall = column.scrollHeight;
        column.scrollTop = tall;
        return { tall, window: innerHeight };
      });
      expect(span, `${row}: no content column to scroll`).not.toBeNull();
      if (options.end === "all") {
        // Its top and its end cover all of it only if it's at most twice the window.
        expect(span?.tall ?? 0, `${row}: taller than two windows`).toBeLessThanOrEqual(
          2 * (span?.window ?? 0),
        );
      }
      await inBothThemes(page, `${name}-end`, options);
      await page.evaluate(() => {
        document.querySelector("[data-slot=content]")?.scrollTo({ top: 0 });
      });
    }
    if (options.small === true) {
      await page.setViewportSize({
        width: SMALL.width,
        height: options.smallHeight ?? SMALL.height,
      });
      await laidOut(page);
      if (options.keepPointer !== true) {
        await park(page);
      }
      await inBothThemes(page, `${name}-small`, options);
    }
  } finally {
    await restore();
    await page.setViewportSize({ width, height });
  }
}
