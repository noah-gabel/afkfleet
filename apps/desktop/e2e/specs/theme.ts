// What the theme's specs do on a page (Plan.md P11.10; ADR-0020, S96).
import type { Page } from "@playwright/test";

import { walk } from "../contrast";
import { expect } from "../fixtures";

/** The floor every text holds in both themes; AA is the design pass's (S97). */
export const FLOOR = 3;

/** What a theme is called in the media query and in `<html>`'s class. */
export type Scheme = "light" | "dark";

/** Waits until Windows' setting says `scheme` and `<html>` follows it, animations finished. */
export async function inScheme(page: Page, scheme: Scheme): Promise<void> {
  await page.emulateMedia({ colorScheme: scheme });
  await expect(page.locator("html")).toHaveClass(new RegExp(`\\b${scheme}\\b`));
  await page.evaluate(() =>
    Promise.all(
      document.getAnimations().flatMap((animation) => {
        if (animation.effect?.getComputedTiming().endTime === Number.POSITIVE_INFINITY) {
          return [];
        }
        animation.finish();
        return [animation.finished];
      }),
    ),
  );
}

/** Holds what's on the page against the floor in the theme it's in, naming `row` when it fails. */
export async function measured(page: Page, row: string, scheme: Scheme): Promise<void> {
  const found = await page.evaluate(walk, FLOOR);
  expect(found.walked, `${row} in ${scheme}: nothing was measured`).toBeGreaterThan(0);
  expect(found.low, `${row} in ${scheme}`).toEqual([]);
  const colorScheme = await page.evaluate(
    () => getComputedStyle(document.documentElement).colorScheme,
  );
  expect(colorScheme, `${row} in ${scheme}`).toBe(scheme);
}

/**
 * Every native select on the page, and its options, hold the theme's tokens for `scheme`: what
 * WebView2 paints the open list from. The list itself isn't in the DOM, so that stays a hands-on
 * check. Answers how many selects it read.
 */
export async function selectsInTokens(page: Page, row: string, scheme: Scheme): Promise<number> {
  await inScheme(page, scheme);
  const { tokens, found } = await page.evaluate(() => {
    const probe = document.createElement("div");
    probe.style.background = "var(--background)";
    probe.style.color = "var(--foreground)";
    document.body.append(probe);
    const held = getComputedStyle(probe);
    const tokens = { background: held.backgroundColor, color: held.color };
    probe.remove();
    const read = (element: Element) => {
      const style = getComputedStyle(element);
      return { background: style.backgroundColor, color: style.color };
    };
    return {
      tokens,
      found: [...document.querySelectorAll("select, select option")].map(read),
    };
  });
  for (const colours of found) {
    expect(colours, `${row} in ${scheme}`).toEqual(tokens);
  }
  return found.length;
}

/** What's on the page, in both themes, flipped in place. */
export async function bothThemes(page: Page, row: string): Promise<void> {
  for (const scheme of ["dark", "light"] as const) {
    await inScheme(page, scheme);
    await measured(page, row, scheme);
  }
}
