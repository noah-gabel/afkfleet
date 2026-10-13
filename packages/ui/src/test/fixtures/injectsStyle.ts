// A test-only module that does what the release CSP blocks: it adds a <style> element
// at import time, as sonner 2.0.8 does unpatched. Only styleGuard.test.ts imports it.
const style = document.createElement("style");
style.textContent = ".injected { color: red; }";
document.head.appendChild(style);

/** The element this module added, so the test can remove it again. */
export const injected = style;
