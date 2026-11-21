import { afterEach, describe, expect, it } from "vitest";

import { assertNoInjectedStyles, SCRIPT_EDITOR_HOST } from "./styleGuard";

describe("assertNoInjectedStyles", () => {
  afterEach(() => {
    for (const element of document.querySelectorAll("style")) {
      element.remove();
    }
    document.body.replaceChildren();
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

  describe("shadow roots (ADR-0045)", () => {
    /** A host in the body with an open shadow root; `editor` marks it as the script editor's. */
    function hosted(editor: boolean): ShadowRoot {
      const host = document.createElement("div");
      if (editor) {
        host.dataset.slot = "script-editor";
      }
      document.body.append(host);
      expect(host.matches(SCRIPT_EDITOR_HOST)).toBe(editor);
      return host.attachShadow({ mode: "open" });
    }

    it("lets the script editor's shadow root adopt a constructed stylesheet", () => {
      hosted(true).adoptedStyleSheets = [new CSSStyleSheet()];

      expect(() => assertNoInjectedStyles(document)).not.toThrow();
    });

    it("throws when another shadow root adopts one, nested ones included", () => {
      const outer = hosted(false);
      const inner = document.createElement("div");
      outer.append(inner);
      inner.attachShadow({ mode: "open" }).adoptedStyleSheets = [new CSSStyleSheet()];

      expect(() => assertNoInjectedStyles(document)).toThrow(/isn't the script editor's/);
    });

    it("throws for a <style> in any shadow root, the script editor's too", () => {
      hosted(true).append(document.createElement("style"));

      expect(() => assertNoInjectedStyles(document)).toThrow(/<style>.*shadow root/);
    });
  });
});
