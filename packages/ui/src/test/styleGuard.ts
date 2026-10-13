// The release CSP has `style-src 'self'` without 'unsafe-inline' (Plan.md P8.3, ADR-0017),
// so a <style> element or a constructed stylesheet that a library adds at runtime is
// blocked in the real app, while `just dev-app` (no CSP) shows nothing wrong. Every test
// therefore ends with this check (setup.ts): the document must hold no <style>, no
// stylesheet and no adopted stylesheet at all, whoever added it and when. A library that
// injects at import time is caught too, since its element is still there after the first
// test of every file that imports it.

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
  // jsdom 29 has no `adoptedStyleSheets`; a library that assigns one creates it.
  const adopted: unknown = Reflect.get(document, "adoptedStyleSheets");
  if (Array.isArray(adopted) && adopted.length > 0) {
    throw new Error(
      `${adopted.length} adopted stylesheet(s) in the document: the release CSP (style-src 'self') blocks them`,
    );
  }
}
