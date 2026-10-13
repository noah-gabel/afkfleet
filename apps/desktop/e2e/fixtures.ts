// The e2e specs' fixtures (Plan.md P8.9, ADR-0017).
// - `fakeBackend(context)`: a fresh FakeBackend, answering the page's commands through MSW
//   on Playwright's routing (@msw/playwright). Assets and the page itself pass through to
//   `vite preview`.
// - The guards, on every browser spec: an init script records every CSP violation, then
//   freezes `Object.prototype` as Tauri's `freezePrototype` does. At the end the page must
//   hold no `<style>` element, no adopted stylesheet and no stylesheet from another origin,
//   and there must have been no violation, no console error and no uncaught error. The page
//   is served with the release CSP (e2e/csp.ts), so a library that injects a style or
//   evaluates code fails here, as it would in the app.
import { defineNetworkFixture } from "@msw/playwright";
import { type BrowserContext, test as base, expect } from "@playwright/test";

import { FakeBackend } from "./fake/backend";
import { handlers } from "./fake/handlers";

/** A fresh fake backend that answers the commands of `context`'s pages. */
export async function fakeBackend(context: BrowserContext): Promise<FakeBackend> {
  const backend = new FakeBackend();
  const network = defineNetworkFixture({
    context,
    handlers: handlers(backend),
    onUnhandledFrame: "bypass",
  });
  await network.enable();
  return backend;
}

/** What the guards find on the page at the end. */
interface Found {
  readonly violations: readonly string[];
  readonly styles: number;
  readonly adopted: number;
  readonly foreignSheets: number;
}

/** The specs' `test`, with the guards on every page. */
export const test = base.extend<{ guards: undefined }>({
  guards: [
    async ({ page }, use) => {
      const errors: string[] = [];
      page.on("console", (message) => {
        if (message.type() === "error") {
          errors.push(message.text());
        }
      });
      page.on("pageerror", (error) => errors.push(error.message));
      await page.addInitScript(() => {
        const seen: string[] = [];
        Object.defineProperty(window, "__cspViolations", { value: seen });
        document.addEventListener("securitypolicyviolation", (event) => {
          seen.push(`${event.violatedDirective} ${event.blockedURI}`);
        });
        Object.freeze(Object.prototype);
      });

      await use(undefined);

      const found: Found = await page.evaluate(() => ({
        violations: (window as unknown as { __cspViolations?: string[] }).__cspViolations ?? [],
        styles: document.querySelectorAll("style").length,
        adopted: document.adoptedStyleSheets.length,
        foreignSheets: [...document.styleSheets].filter(
          (sheet) => sheet.href === null || new URL(sheet.href).origin !== location.origin,
        ).length,
      }));
      expect(found).toEqual({ violations: [], styles: 0, adopted: 0, foreignSheets: 0 });
      expect(errors).toEqual([]);
    },
    { auto: true },
  ],
});

export { expect };
