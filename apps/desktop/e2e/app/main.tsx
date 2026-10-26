// The e2e build's entry (Plan.md P8.9, ADR-0017): the desktop app's UI with its real
// TauriApiClient, over fetchInvoke instead of Tauri's IPC, so Playwright drives what ships,
// minus Tauri. The fake holds the live events watch; a spec pushes its updates through the
// e2e build's hook (push.ts). Only vite.e2e.config.ts builds it, into dist-e2e/, never the
// app's own build.
import "@afkfleet/ui/styles.css";

import { App } from "@afkfleet/ui";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

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
createRoot(root).render(
  <StrictMode>
    <App apiClient={new TauriApiClient(fetchInvoke(), openChannel)} />
  </StrictMode>,
);
