// The sweep's check of what a page shows (Plan.md P11.11; ADR-0020, S96, S107, S108). `probe` runs
// in the page, so it holds its whole self; theme.ts drives it.
// - Text: every visible text, control value and placeholder, its computed colour against the
//   backgrounds behind it, composited, at WCAG AA: 4.5:1, or 3:1 for large text (24 px, or 18.66 px
//   at weight 700 or more).
// - Focus: a real Tab through the page, each stop's computed outline at 3:1 against what's behind
//   it. Only the outline counts, so a stop whose indicator is a shadow or a border fails: one
//   stylesheet rule draws them all (styles.css). `outline-style: auto` fails too, since its painted
//   colour can't be read.
//
// **Left out, on purpose:** WCAG 1.4.11's graphics (a status square, an icon, an input's border, a
// radio's mark), hover and pressed colours, generated content, and a select's open list, which
// isn't in the DOM.

/** A text below its floor. */
export interface LowContrast {
  readonly text: string;
  readonly color: string;
  readonly background: string;
  readonly ratio: number;
  /** The floor it missed. */
  readonly floor: number;
}

/** What a walk over the texts found. */
export interface Walked {
  /** How many texts it measured: a row that measures none proves nothing. */
  readonly walked: number;
  readonly low: readonly LowContrast[];
}

/** The floors a text holds: `large` for large text. */
export interface Floors {
  readonly normal: number;
  readonly large: number;
}

/** One Tab's reading: what's wrong with this stop, if anything, and whether the walk ended. */
export interface TabStep {
  readonly done: boolean;
  readonly problem?: string;
}

/** How a Tab walk ended: what it should have reached, and what it disturbed. */
export interface TabEnd {
  readonly missed: readonly string[];
  readonly changed: readonly string[];
}

/** What the page can be asked. */
export type Probe =
  | { readonly kind: "text"; readonly floors: Floors }
  | { readonly kind: "tabStart" }
  | { readonly kind: "tabStep" }
  | { readonly kind: "tabEnd" };

/** Answers `request` in the page; see the module comment. */
export function probe(request: Probe): Walked | number | TabStep | TabEnd {
  type Rgba = readonly [number, number, number, number];
  const canvas = document.createElement("canvas");
  canvas.width = 1;
  canvas.height = 1;
  const context = canvas.getContext("2d", { willReadFrequently: true });
  if (context === null) {
    throw new Error("no 2D context");
  }
  const read = (start: string, color: string) => {
    context.clearRect(0, 0, 1, 1);
    context.fillStyle = start;
    context.fillStyle = color;
    context.fillRect(0, 0, 1, 1);
    return [...context.getImageData(0, 0, 1, 1).data];
  };
  /**
   * `color` as sRGB through the canvas, since computed tokens read as `oklch(...)`. A colour the
   * canvas can't parse leaves its last colour, which would pass as black: two starts show it.
   */
  const rgba = (color: string): Rgba => {
    const [r = 0, g = 0, b = 0, a = 0] = read("#000", color);
    if (read("#fff", color).join() !== [r, g, b, a].join()) {
      throw new Error(`a colour the canvas can't read: ${color}`);
    }
    return [r, g, b, a / 255];
  };
  const over = (top: Rgba, under: Rgba): Rgba => {
    const [r, g, b, a] = top;
    return [r * a + under[0] * (1 - a), g * a + under[1] * (1 - a), b * a + under[2] * (1 - a), 1];
  };
  const linear = (channel: number) => {
    const value = channel / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  };
  const luminance = ([r, g, b]: Rgba) =>
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
  const ratioOf = (a: Rgba, b: Rgba) => {
    const [high, low] = [luminance(a), luminance(b)].sort((x, y) => y - x) as [number, number];
    return (high + 0.05) / (low + 0.05);
  };
  const rounded = (ratio: number) => Math.round(ratio * 100) / 100;
  /** The backgrounds behind `element`, composited down to the first opaque one. */
  const backdrop = (element: Element): Rgba => {
    const layers: Rgba[] = [];
    for (let at: Element | null = element; at !== null; at = at.parentElement) {
      const layer = rgba(getComputedStyle(at).backgroundColor);
      if (layer[3] > 0) {
        layers.push(layer);
      }
      if (layer[3] === 1) {
        break;
      }
    }
    return layers.reduceRight<Rgba>((under, top) => over(top, under), [255, 255, 255, 1]);
  };
  /** The opacity `element` and its ancestors multiply to. */
  const opacity = (element: Element): number => {
    let total = 1;
    for (let at: Element | null = element; at !== null; at = at.parentElement) {
      total *= Number(getComputedStyle(at).opacity);
    }
    return total;
  };
  const shown = (element: Element) => {
    const box = element.getBoundingClientRect();
    return (
      element.checkVisibility({ opacityProperty: true, visibilityProperty: true }) &&
      box.width > 1 &&
      box.height > 1
    );
  };
  const disabled = (element: Element) =>
    element.closest(":disabled, [aria-disabled='true'], [data-disabled]") !== null;

  const walkTexts = (floors: Floors): Walked => {
    let walked = 0;
    const low: LowContrast[] = [];
    const measure = (element: Element, text: string, pseudo?: string) => {
      if (text === "" || !shown(element) || disabled(element)) {
        return;
      }
      walked += 1;
      const background = backdrop(element);
      const style = getComputedStyle(element);
      const painted = pseudo === undefined ? style : getComputedStyle(element, pseudo);
      const [r, g, b, a] = rgba(painted.color);
      const faded = pseudo === undefined ? 1 : Number(painted.opacity);
      const color = over([r, g, b, a * opacity(element) * faded], background);
      const ratio = ratioOf(color, background);
      const size = Number.parseFloat(style.fontSize);
      const large = size >= 24 || (size >= 18.66 && Number(style.fontWeight) >= 700);
      const floor = large ? floors.large : floors.normal;
      if (ratio < floor) {
        low.push({
          text: text.slice(0, 60),
          color: painted.color,
          background: `rgb(${background.slice(0, 3).map(Math.round).join(" ")})`,
          ratio: rounded(ratio),
          floor,
        });
      }
    };

    const texts = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    for (let node = texts.nextNode(); node !== null; node = texts.nextNode()) {
      const parent = node.parentElement;
      if (parent !== null && parent.closest("script, style, option, select, textarea") === null) {
        measure(parent, (node.textContent ?? "").trim());
      }
    }
    for (const control of document.querySelectorAll<HTMLInputElement | HTMLSelectElement>(
      "input, select, textarea",
    )) {
      const type = control.getAttribute("type");
      if (type !== "radio" && type !== "checkbox" && type !== "hidden") {
        measure(control, control.value);
        if (control.value === "") {
          measure(control, control.getAttribute("placeholder") ?? "", "::placeholder");
        }
      }
    }
    return { walked, low };
  };

  /** How a stop is named in a failure. */
  const nameOf = (element: Element) => {
    const label = element.getAttribute("aria-label") ?? (element.textContent ?? "").trim();
    return `<${element.tagName.toLowerCase()}> ${label.slice(0, 40)}`;
  };

  /** What's wrong with the outline of `element`, the focused one, or undefined. */
  const outlineProblem = (element: Element): string | undefined => {
    const style = getComputedStyle(element);
    const width = Number.parseFloat(style.outlineWidth);
    if (style.outlineStyle === "none" || style.outlineStyle === "auto" || width === 0) {
      return `outline ${style.outlineStyle} ${style.outlineWidth}`;
    }
    const offset = Number.parseFloat(style.outlineOffset);
    const [r, g, b, a] = rgba(style.outlineColor);
    const alpha = a * opacity(element);
    const box = element.getBoundingClientRect();
    const out = offset + width / 2;
    const middle = { x: box.left + box.width / 2, y: box.top + box.height / 2 };
    const sides: readonly (readonly [number, number])[] = [
      [box.left - out, middle.y],
      [box.right + out, middle.y],
      [middle.x, box.top - out],
      [middle.x, box.bottom + out],
    ];
    // What floats over the page covers the outline; it isn't what the outline is drawn on.
    const FLOATING = "[data-sonner-toaster], [data-slot=tooltip]";
    let lowest = Number.POSITIVE_INFINITY;
    let lowestOn = element;
    for (const [x, y] of sides) {
      // An inset outline lies on the element's own fill; off the screen, its parent's is behind.
      const behind =
        offset < 0
          ? element
          : (document
              .elementsFromPoint(x, y)
              .find(
                (hit) =>
                  hit !== element && (hit.closest(FLOATING) === null || element.closest(FLOATING)),
              ) ??
            element.parentElement ??
            element);
      const under = backdrop(behind);
      const ratio = ratioOf(over([r, g, b, alpha], under), under);
      if (ratio < lowest) {
        lowest = ratio;
        lowestOn = behind;
      }
    }
    return lowest < 3
      ? `outline ${style.outlineColor} at ${rounded(lowest)}:1 against what's behind it, ${nameOf(lowestOn)}`
      : undefined;
  };

  /** The state of a Tab walk, kept on the window between its calls. */
  interface Walk {
    readonly returnTo: Element | null;
    readonly scrolls: readonly (readonly [Element, number, number])[];
    readonly counts: string;
    readonly pending: Map<Element | string, string>;
    readonly seen: Set<Element>;
    anchor: Element | null;
    bodies: number;
  }
  const holder = window as unknown as { __focusWalk?: Walk };
  const keyOf = (element: Element): Element | string =>
    element instanceof HTMLInputElement && element.type === "radio" && element.name !== ""
      ? `radio ${element.name}`
      : element;
  /** What a row shows that a Tab through it mustn't change. */
  const counts = () =>
    ["[role=alert]", "[aria-invalid=true]", "[data-slot=tooltip]"]
      .map((selector) => `${selector} ${document.querySelectorAll(selector).length}`)
      .join(", ");

  switch (request.kind) {
    case "text":
      return walkTexts(request.floors);
    case "tabStart": {
      const pending = new Map<Element | string, string>();
      for (const element of document.querySelectorAll(
        "a[href], button, input, select, textarea, summary, [tabindex]",
      )) {
        if (!element.matches(":disabled, [tabindex='-1'], input[type=hidden]") && shown(element)) {
          pending.set(keyOf(element), nameOf(element));
        }
      }
      holder.__focusWalk = {
        returnTo: document.activeElement,
        scrolls: [...document.querySelectorAll("*")]
          .filter((element) => element.scrollHeight > element.clientHeight)
          .map((element) => [element, element.scrollTop, element.scrollLeft] as const),
        counts: counts(),
        pending,
        seen: new Set(),
        anchor: null,
        bodies: 0,
      };
      return pending.size;
    }
    case "tabStep": {
      const walk = holder.__focusWalk;
      if (walk === undefined) {
        throw new Error("no Tab walk started");
      }
      const active = document.activeElement;
      if (active === null || active === document.body || active === document.documentElement) {
        walk.bodies += 1;
        return { done: walk.bodies >= 3 };
      }
      walk.bodies = 0;
      if (walk.anchor !== null && !walk.anchor.isConnected) {
        walk.anchor = null;
      }
      if (active === walk.anchor) {
        return { done: true };
      }
      walk.anchor ??= active;
      if (walk.seen.has(active)) {
        return { done: true, problem: `focus is trapped at ${nameOf(active)}` };
      }
      walk.seen.add(active);
      walk.pending.delete(keyOf(active));
      // Read each stop as it ends up: mid-transition, a colour or a toast's opacity is on its way.
      for (const animation of document.getAnimations()) {
        if (animation.effect?.getComputedTiming().endTime !== Number.POSITIVE_INFINITY) {
          animation.finish();
        }
      }
      if (!shown(active)) {
        // A toast on its way out, or behind the stack: sonner keeps it in the tab order.
        return active.closest("[data-sonner-toast]") === null
          ? { done: false, problem: `a stop that isn't shown: ${nameOf(active)}` }
          : { done: false };
      }
      const problem = outlineProblem(active);
      return problem === undefined
        ? { done: false }
        : { done: false, problem: `${nameOf(active)}: ${problem}` };
    }
    case "tabEnd": {
      const walk = holder.__focusWalk;
      if (walk === undefined) {
        throw new Error("no Tab walk started");
      }
      delete holder.__focusWalk;
      if (walk.returnTo instanceof HTMLElement && walk.returnTo.isConnected) {
        walk.returnTo.focus({ preventScroll: true });
      } else if (document.activeElement instanceof HTMLElement) {
        document.activeElement.blur();
      }
      for (const [element, top, left] of walk.scrolls) {
        element.scrollTop = top;
        element.scrollLeft = left;
      }
      const now = counts();
      return {
        missed: [...walk.pending]
          .filter(([key]) => typeof key === "string" || key.isConnected)
          .map(([, name]) => `Tab never reached ${name}`),
        changed: now === walk.counts ? [] : [`the Tab changed the row: ${walk.counts} → ${now}`],
      };
    }
  }
}
