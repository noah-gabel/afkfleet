// What the theme's specs do on a page (Plan.md P11.10; ADR-0020, S96). A sweep row measures in Edge
// and, in the screenshots' project, is its own baseline instead (U125, R226).
import type { Page } from "@playwright/test";

import {
  type Floors,
  type LogoPart,
  type Probe,
  probe,
  type TabEnd,
  type TabStep,
  type Walked,
} from "../contrast";
import { expect, shooting } from "../fixtures";
import { inScheme, type Scheme } from "./scheme";
import { type ShotOptions, shoot } from "./shoot";

export { inScheme, type Scheme } from "./scheme";

/** WCAG AA, which every text holds in both themes (ADR-0020, S97, S108). */
export const AA: Floors = { normal: 4.5, large: 3 };

/** Asks the page, as `T`: the answer that `request`'s kind gives. */
async function asked<T>(page: Page, request: Probe): Promise<T> {
  return (await page.evaluate(probe as (request: Probe) => unknown, request)) as T;
}

/** The texts on the page against `floors`. */
export function walked(page: Page, floors: Floors): Promise<Walked> {
  return asked<Walked>(page, { kind: "text", floors });
}

/**
 * Tabs through every focusable control on the page, holding each stop's outline, or a marked stop's
 * cells, at 3:1, and leaves the page as it was. Answers what's wrong: a low or missing indicator, a
 * trap, a tabbable it never reached, a cap it hit, or a change to the row.
 */
export async function focusProblems(page: Page): Promise<readonly string[]> {
  const problems: string[] = [];
  const tabbables = await asked<number>(page, { kind: "tabStart" });
  let done = false;
  for (let stop = 0; !done && stop < tabbables + 20; stop += 1) {
    await page.keyboard.press("Tab");
    const step = await asked<TabStep>(page, { kind: "tabStep" });
    done = step.done;
    if (step.problem !== undefined) {
      problems.push(step.problem);
    }
  }
  if (!done) {
    problems.push(`the Tab walk passed its cap of ${tabbables + 20} stops`);
  }
  const end = await asked<TabEnd>(page, { kind: "tabEnd" });
  return [...problems, ...end.missed, ...end.changed, ...end.unchanged];
}

/** The logo's three parts, ring, dot and satellite, against what's behind it. */
export function logoParts(page: Page): Promise<readonly LogoPart[]> {
  return asked<readonly LogoPart[]>(page, { kind: "logo" });
}

/**
 * Holds what's on the page to AA in the theme it's in, its texts and each focus stop, naming `row`
 * when it fails.
 */
export async function measured(page: Page, row: string, scheme: Scheme): Promise<void> {
  const found = await walked(page, AA);
  expect(found.walked, `${row} in ${scheme}: nothing was measured`).toBeGreaterThan(0);
  expect(found.low, `${row} in ${scheme}`).toEqual([]);
  expect(await focusProblems(page), `${row} in ${scheme}: its focus stops`).toEqual([]);
  const colorScheme = await page.evaluate(
    () => getComputedStyle(document.documentElement).colorScheme,
  );
  expect(colorScheme, `${row} in ${scheme}`).toBe(scheme);
}

/**
 * Only the texts on the page, in both themes: for a row a Tab would change, as it should; then the
 * frame (R225). In the
 * screenshots' project, the row's baselines instead, as `shot` asks.
 */
export async function textsInBothThemes(
  page: Page,
  row: string,
  shot: ShotOptions = {},
): Promise<void> {
  if (shooting()) {
    await shoot(page, row, shot);
    return;
  }
  for (const scheme of ["dark", "light"] as const) {
    await inScheme(page, scheme);
    const found = await walked(page, AA);
    expect(found.walked, `${row} in ${scheme}: nothing was measured`).toBeGreaterThan(0);
    expect(found.low, `${row} in ${scheme}`).toEqual([]);
  }
  await frameHolds(page, row);
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

/**
 * What's on the page, in both themes, flipped in place, then the frame (R225, R227): after every
 * scroller went to its end, the page never scrolls and Back stays. In the screenshots' project, the row's
 * baselines instead, as `shot` asks.
 */
export async function bothThemes(page: Page, row: string, shot: ShotOptions = {}): Promise<void> {
  if (shooting()) {
    await shoot(page, row, shot);
    return;
  }
  for (const scheme of ["dark", "light"] as const) {
    await inScheme(page, scheme);
    await measured(page, row, scheme);
  }
  await frameHolds(page, row);
}

/** What the frame does after every scroller on the screen went to its end. */
export interface Frame {
  /** Whether each of the page's own boxes scrolls: never `html`, `body` or `#root`. */
  readonly own: {
    readonly html: boolean;
    readonly body: boolean;
    readonly root: boolean;
    readonly screen: boolean;
    readonly outer: boolean;
  };
  /** Whether a signed-in frame is showing, whose screen and outer column never scroll. */
  readonly signedIn: boolean;
  /** Where Back's row was before and after. */
  readonly backBefore: number | undefined;
  readonly backAfter: number | undefined;
  /** What ends below the window outside a scroller that holds it, up to five. */
  readonly escaped: readonly string[];
}

/**
 * The frame after every scroller went to its end (ADR-0020, R225): whether the page's own boxes
 * scroll, where Back's row is, and every element that ends below the window outside a scroller that
 * holds it, found up its containing blocks (an absolute element skips the boxes that aren't
 * positioned). Every scroller then goes back where it was.
 */
export const frameAtTheEnd = (page: Page): Promise<Frame> =>
  page.evaluate(() => {
    const scrolls = (element: Element | null | undefined) =>
      element !== null && element !== undefined && element.scrollHeight > element.clientHeight;
    const content = document.querySelector("[data-slot=content]");
    const back = () => document.querySelector("[data-slot=back-row]")?.getBoundingClientRect().top;
    const backBefore = back();
    const own = {
      html: scrolls(document.documentElement),
      body: scrolls(document.body),
      root: scrolls(document.getElementById("root")),
      screen: scrolls(document.querySelector("[data-slot=screen]")),
      outer: scrolls(content?.parentElement),
    };
    const every = [document.documentElement, ...document.body.querySelectorAll("*")];
    const were = every.map((element) => element.scrollTop);
    for (const element of every) {
      element.scrollTop = element.scrollHeight;
    }
    const containing = (element: Element): Element | null => {
      const position = getComputedStyle(element).position;
      if (position === "fixed") {
        return null;
      }
      let up = element.parentElement;
      if (position === "absolute") {
        while (up !== null && getComputedStyle(up).position === "static") {
          up = up.parentElement;
        }
      }
      return up;
    };
    const held = (element: Element) => {
      for (let up = containing(element); up !== null; up = containing(up)) {
        if (getComputedStyle(up).overflowY !== "visible") {
          return up.getBoundingClientRect().bottom <= innerHeight + 1;
        }
      }
      return false;
    };
    const escaped: string[] = [];
    for (const element of document.body.querySelectorAll("*")) {
      const box = element.getBoundingClientRect();
      if ((box.width > 0 || box.height > 0) && box.bottom > innerHeight + 1 && !held(element)) {
        escaped.push(`${element.tagName.toLowerCase()} "${element.textContent?.slice(0, 30)}"`);
      }
    }
    const backAfter = back();
    every.forEach((element, at) => {
      element.scrollTop = were[at] ?? 0;
    });
    return { own, signedIn: content !== null, backBefore, backAfter, escaped: escaped.slice(0, 5) };
  });

/**
 * R225's lesson for `row`: after every scroller went to its end, the page never scrolls, nor a
 * signed-in frame's screen and outer column, Back's row stays where it was, and nothing escapes its
 * scrollers.
 */
export async function frameHolds(page: Page, row: string): Promise<void> {
  const frame = await frameAtTheEnd(page);
  const { html, body, root, screen, outer } = frame.own;
  expect({ html, body, root }, `${row}: the page scrolls`).toEqual({
    html: false,
    body: false,
    root: false,
  });
  if (frame.signedIn) {
    expect({ screen, outer }, `${row}: the frame scrolls`).toEqual({ screen: false, outer: false });
  }
  expect(frame.backAfter, `${row}: Back's row moved`).toBe(frame.backBefore);
  expect(frame.escaped, `${row}: escaped its scrollers`).toEqual([]);
}
