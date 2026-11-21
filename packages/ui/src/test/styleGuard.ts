// The release CSP has `style-src 'self'` without 'unsafe-inline' (Plan.md P8.3, ADR-0017),
// so a <style> element or a constructed stylesheet that a library adds at runtime is
// blocked in the real app, while `just dev-app` (no CSP) shows nothing wrong. Every test
// therefore ends with this check (setup.ts): the document must hold no <style>, no
// stylesheet and no adopted stylesheet at all, whoever added it and when. A library that
// injects at import time is caught too, since its element is still there after the first
// test of every file that imports it.
//
// **The one exception** (ADR-0036, D16; ADR-0045): the script editor's shadow root, whose host
// is `[data-slot="script-editor"]`, may adopt constructed stylesheets: CodeMirror's style-mod
// writes its styles there, and Edge's CSP allows a constructed sheet. Every other shadow root
// is held to the document's rule, and no shadow root may hold a <style>, the editor's included.

/** The host of the one shadow root that may adopt stylesheets. */
export const SCRIPT_EDITOR_HOST = '[data-slot="script-editor"]';

/** Every open shadow root under `root`, nested ones included. */
function shadowRoots(root: Document | ShadowRoot): ShadowRoot[] {
  const found: ShadowRoot[] = [];
  for (const element of root.querySelectorAll("*")) {
    if (element.shadowRoot !== null) {
      found.push(element.shadowRoot, ...shadowRoots(element.shadowRoot));
    }
  }
  return found;
}

/** How many stylesheets `holder` has adopted; jsdom 29 has none of its own. */
function adoptedCount(holder: Document | ShadowRoot): number {
  const adopted: unknown = Reflect.get(holder, "adoptedStyleSheets");
  return Array.isArray(adopted) ? adopted.length : 0;
}

/** Throws unless the document holds no injected styles; see the module comment. */
export function assertNoInjectedStyles(document: Document): void {
  const elements = document.querySelectorAll("style").length;
  if (elements > 0) {
    throw new Error(
      `${elements} <style> element(s) in the document: the release CSP (style-src 'self') blocks them`,
    );
  }
  const sheets = document.styleSheets.length;
  if (sheets > 0) {
    throw new Error(
      `${sheets} stylesheet(s) in the document: the UI's styles come only from its bundle`,
    );
  }
  const adopted = adoptedCount(document);
  if (adopted > 0) {
    throw new Error(
      `${adopted} adopted stylesheet(s) in the document: the release CSP (style-src 'self') blocks them`,
    );
  }
  for (const root of shadowRoots(document)) {
    const inRoot = root.querySelectorAll("style").length;
    if (inRoot > 0) {
      throw new Error(
        `${inRoot} <style> element(s) in a shadow root: the release CSP (style-src 'self') blocks them`,
      );
    }
    if (adoptedCount(root) > 0 && !root.host.matches(SCRIPT_EDITOR_HOST)) {
      throw new Error(
        `${adoptedCount(root)} adopted stylesheet(s) in a shadow root that isn't the script editor's: only its root may adopt one`,
      );
    }
  }
}
