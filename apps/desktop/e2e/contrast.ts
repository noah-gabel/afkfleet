// The sweep's check of what a page shows (Plan.md P11.10; ADR-0020, S96): every visible text and
// control value, its computed colour against the backgrounds behind it, composited. It runs in
// the page, so it holds its whole self. The floor is 3:1, for what a theme mustn't break; WCAG AA
// is the design pass's (S97).

/** A text below the floor. */
export interface LowContrast {
  readonly text: string;
  readonly color: string;
  readonly background: string;
  readonly ratio: number;
}

/** What a walk found. */
export interface Walked {
  /** How many texts it measured: a row that measures none proves nothing. */
  readonly walked: number;
  readonly low: readonly LowContrast[];
}

/** Measures the page against `floor`; see the module comment. */
export function walk(floor: number): Walked {
  type Rgba = readonly [number, number, number, number];
  const canvas = document.createElement("canvas");
  canvas.width = 1;
  canvas.height = 1;
  const context = canvas.getContext("2d", { willReadFrequently: true });
  if (context === null) {
    throw new Error("no 2D context");
  }
  /** `color` as sRGB through the canvas, since computed tokens read as `oklch(...)`. */
  const rgba = (color: string): Rgba => {
    context.clearRect(0, 0, 1, 1);
    context.fillStyle = "#000";
    context.fillStyle = color;
    context.fillRect(0, 0, 1, 1);
    const [r = 0, g = 0, b = 0, a = 0] = context.getImageData(0, 0, 1, 1).data;
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

  let walked = 0;
  const low: LowContrast[] = [];
  const measure = (element: Element, text: string) => {
    if (text === "" || !shown(element) || disabled(element)) {
      return;
    }
    walked += 1;
    const background = backdrop(element);
    const style = getComputedStyle(element);
    const [r, g, b, a] = rgba(style.color);
    const color = over([r, g, b, a * opacity(element)], background);
    const ratio = ratioOf(color, background);
    if (ratio < floor) {
      low.push({
        text: text.slice(0, 60),
        color: style.color,
        background: `rgb(${background.slice(0, 3).map(Math.round).join(" ")})`,
        ratio: Math.round(ratio * 100) / 100,
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
    }
  }
  return { walked, low };
}
