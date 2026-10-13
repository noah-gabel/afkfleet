import { afterEach, describe, expect, it } from "vitest";

import { assertNoInjectedStyles } from "./styleGuard";

describe("assertNoInjectedStyles", () => {
  afterEach(() => {
    for (const element of document.querySelectorAll("style")) {
      element.remove();
    }
    Reflect.deleteProperty(document, "adoptedStyleSheets");
  });

  it("passes on a document without styles", () => {
    expect(() => assertNoInjectedStyles(document)).not.toThrow();
  });

  it("throws when a module added a <style> at import time", async () => {
    const { injected } = await import("./fixtures/injectsStyle");

    expect(() => assertNoInjectedStyles(document)).toThrow(/<style>/);
    injected.remove();
  });

  it("throws when a <style> is rendered inside the body", () => {
    document.body.appendChild(document.createElement("style"));

    expect(() => assertNoInjectedStyles(document)).toThrow(/<style>/);
  });

  it("throws when a stylesheet was adopted", () => {
    Object.defineProperty(document, "adoptedStyleSheets", {
      configurable: true,
      value: [new CSSStyleSheet()],
    });

    expect(() => assertNoInjectedStyles(document)).toThrow(/adopted/);
  });
});
