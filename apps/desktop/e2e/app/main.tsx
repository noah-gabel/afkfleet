// The e2e build's entry (Plan.md P8.9, ADR-0017): the desktop app's UI with its real
// TauriApiClient, over fetchInvoke instead of Tauri's IPC, so Playwright drives what ships,
// minus Tauri. The fake holds the live events watch; a spec pushes its updates through the
// e2e build's hook (push.ts). `?panel=error` shows the real error screen, which the theme's sweep
// measures, since reaching it through the error boundary logs a console error the fixtures refuse
// (ADR-0020, S96). Only vite.e2e.config.ts builds it, into dist-e2e/, never the app's own build.
import "@afkfleet/ui/styles.css";

import { App } from "@afkfleet/ui";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { ErrorPanel } from "../../../../packages/ui/src/features/shell/components/ErrorPanel";
import { type OpenChannel, TauriApiClient } from "../../src/TauriApiClient";
import { fetchInvoke } from "./fetchInvoke";
import { E2E_MARKER } from "./marker";
import { pushableChannel } from "./push";

/** The live events' channels, which a spec pushes updates into. */
const openChannel: OpenChannel = pushableChannel();

const root = document.getElementById("root");
if (root === null) {
  throw new Error("e2e/index.html has no #root element");
}
root.dataset.build = E2E_MARKER;
const panel = new URLSearchParams(window.location.search).get("panel") === "error";
createRoot(root).render(
  <StrictMode>
    {panel ? (
      <ErrorPanel error={new Error("the sweep's")} onReset={() => {}} />
    ) : (
      <App apiClient={new TauriApiClient(fetchInvoke(), openChannel)} />
    )}
  </StrictMode>,
);
