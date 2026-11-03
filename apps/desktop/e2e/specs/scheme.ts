// Switching the page's theme (Plan.md P11.10; ADR-0020, S96), for the sweep's measures and its
// screenshots alike.
import type { Page } from "@playwright/test";

import { expect } from "../fixtures";

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
