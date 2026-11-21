// Vitest setup. Vitest runs without globals, so Testing Library's automatic cleanup (which
// needs a global `afterEach`) is registered here, and so is the style guard. Vitest runs
// `afterEach` hooks in reverse order, so the guard (registered last) checks the document
// before cleanup unmounts what the test rendered: a <style> a component renders is caught
// as well as one a library injected. Last of all, every toast is dismissed: sonner keeps
// them in module state and replays the undismissed ones to the next test's Toaster. The theme
// lives outside every App, in `localStorage` and on `<html>`, so each test leaves it as it found it.
import { cleanup } from "@testing-library/react";
import { toast } from "sonner";
import { afterEach, vi } from "vitest";

import { assertNoInjectedStyles } from "./styleGuard";

afterEach(() => {
  toast.dismiss();
  vi.unstubAllGlobals();
  localStorage.clear();
  document.documentElement.classList.remove("light", "dark");
});

afterEach(() => {
  cleanup();
});

afterEach(() => {
  assertNoInjectedStyles(document);
});

// jsdom gives a shadow root no `adoptedStyleSheets`, which Edge and WebView2 have. CodeMirror's
// style-mod would then write a <style> into the script editor's shadow root, which the release
// CSP refuses; with Edge's accessor, tests run the path the app runs: one constructed sheet,
// which the style guard allows only there (ADR-0036, D16; ADR-0045).
const adopted = new WeakMap<ShadowRoot, CSSStyleSheet[]>();
Object.defineProperty(ShadowRoot.prototype, "adoptedStyleSheets", {
  configurable: true,
  get(this: ShadowRoot) {
    return adopted.get(this) ?? [];
  },
  set(this: ShadowRoot, sheets: CSSStyleSheet[]) {
    adopted.set(this, [...sheets]);
  },
});

// jsdom has no document.execCommand, which CodeMirror calls as its editor takes the focus.
document.execCommand = () => false;

// jsdom has no layout, so a Range measures nothing, which CodeMirror asks while it draws.
Range.prototype.getClientRects = () => [] as unknown as DOMRectList;
Range.prototype.getBoundingClientRect = () => new DOMRect();

// jsdom has no layout, so no Element.scrollTo, which the router calls on the content column (S123),
// and no Element.scrollIntoView, which a textarea calls as it takes the focus (ADR-0020, L4).
Element.prototype.scrollTo = () => {};
Element.prototype.scrollIntoView = () => {};
