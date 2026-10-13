// Vitest setup. Vitest runs without globals, so Testing Library's automatic cleanup (which
// needs a global `afterEach`) is registered here, and so is the style guard. Vitest runs
// `afterEach` hooks in reverse order, so the guard (registered last) checks the document
// before cleanup unmounts what the test rendered: a <style> a component renders is caught
// as well as one a library injected. Last of all, every toast is dismissed: sonner keeps
// them in module state and replays the undismissed ones to the next test's Toaster.
import { cleanup } from "@testing-library/react";
import { toast } from "sonner";
import { afterEach } from "vitest";

import { assertNoInjectedStyles } from "./styleGuard";

afterEach(() => {
  toast.dismiss();
});

afterEach(() => {
  cleanup();
});

afterEach(() => {
  assertNoInjectedStyles(document);
});
