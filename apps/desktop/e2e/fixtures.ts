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
// - While the theme sweep's Tab walk runs, the window's own focus and blur reach none of the
//   page's listeners (ADR-0020, R260).
// - In the screenshots' project, the page's clock and the fake's are fixed at `NOW`, so no shot
//   depends on when it ran (ADR-0020, S131).
// - `unanswered(page)`: the commands the page sent that have no answer yet, but those the fake
//   keeps open on purpose. A shot and a change to the fake behind the app's back wait until
//   there is none (specs/steps.ts's `idle`), so neither depends on how fast a read was answered.
import { defineNetworkFixture } from "@msw/playwright";
import {
  type BrowserContext,
  test as base,
  expect,
  type Page,
  type Request,
} from "@playwright/test";

import { IPC_PATH } from "./app/fetchInvoke";
import { BASE_URL } from "./base";
import { FakeBackend } from "./fake/backend";
import { handlers } from "./fake/handlers";
import { NOW, SHOTS_PROJECT } from "./shots";

/** Whether this test runs in the screenshots' project, which shoots rather than measures. */
export function shooting(): boolean {
  return base.info().project.name === SHOTS_PROJECT;
}

/** Where every command goes: `fetchInvoke`'s URL up to the command's name. */
const COMMANDS_URL = `${BASE_URL}${IPC_PATH}`;

/** A context's fake backend, and the commands its pages sent that have no answer yet. */
interface Traffic {
  readonly backend: FakeBackend;
  readonly open: Set<Request>;
}

const TRAFFIC = new WeakMap<BrowserContext, Traffic>();

/**
 * The commands `page` sent that have no answer yet, by name, but those its fake keeps open on
 * purpose (`FakeBackend.keepsOpen`: a held read, the live events watch). None for a page without
 * a fake backend.
 */
export function unanswered(page: Page): readonly string[] {
  const traffic = TRAFFIC.get(page.context());
  if (traffic === undefined) {
    return [];
  }
  return [...traffic.open]
    .map((request) => request.url().slice(COMMANDS_URL.length))
    .filter((command) => !traffic.backend.keepsOpen(command));
}

/** A fresh fake backend that answers the commands of `context`'s pages. */
export async function fakeBackend(context: BrowserContext): Promise<FakeBackend> {
  const backend = new FakeBackend(shooting() ? { now: () => new Date(NOW) } : {});
  const open = new Set<Request>();
  context.on("request", (request) => {
    if (request.url().startsWith(COMMANDS_URL)) {
      open.add(request);
    }
  });
  const over = (request: Request) => {
    open.delete(request);
  };
  context.on("requestfinished", over);
  context.on("requestfailed", over);
  // A command cut off by a new document (a reload, a `goto` right after "Log out") reports no end:
  // its page's open commands end with the document that sent them.
  const endsWithItsDocument = (page: Page) => {
    page.on("framenavigated", (frame) => {
      if (frame === page.mainFrame()) {
        for (const request of open) {
          if (request.frame().page() === page) {
            open.delete(request);
          }
        }
      }
    });
  };
  context.pages().forEach(endsWithItsDocument);
  context.on("page", endsWithItsDocument);
  TRAFFIC.set(context, { backend, open });
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
      if (shooting()) {
        // Before the guards freeze `Object.prototype`, which the clock's own script runs beside.
        await page.clock.setFixedTime(new Date(NOW));
      }
      await page.addInitScript(() => {
        const seen: string[] = [];
        Object.defineProperty(window, "__cspViolations", { value: seen });
        document.addEventListener("securitypolicyviolation", (event) => {
          seen.push(`${event.violatedDirective} ${event.blockedURI}`);
        });
        Object.freeze(Object.prototype);
      });
      // The Tab walk leaves the page at its end and comes back, which the window hears as losing
      // its focus and getting it back. A query read again on focus (the agents', R177) would then
      // change the row after the walk's last stop: the walk's doing, not a Tab's in the row. So
      // while a walk runs (contrast.ts's `__focusWalk`), the window's own focus and blur stop
      // here. A window's own event reaches its listeners in the order they were added, capture
      // or not (Edge, measured), and these come before any of the app's (R260).
      await page.addInitScript(() => {
        const quiet = (event: Event) => {
          if (event.target === window && "__focusWalk" in window) {
            event.stopImmediatePropagation();
          }
        };
        addEventListener("focus", quiet, true);
        addEventListener("blur", quiet, true);
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
