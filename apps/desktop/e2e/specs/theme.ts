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

/** What's on the page, in both themes, flipped in place. */
export async function bothThemes(page: Page, row: string): Promise<void> {
  for (const scheme of ["dark", "light"] as const) {
    await inScheme(page, scheme);
    await measured(page, row, scheme);
  }
}
