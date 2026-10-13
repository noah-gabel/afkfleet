// The e2e build's entry (Plan.md P8.9, ADR-0017): the desktop app's UI with its real
// TauriApiClient, over fetchInvoke instead of Tauri's IPC, so Playwright drives what ships,
// minus Tauri. Only vite.e2e.config.ts builds it, into dist-e2e/, never the app's own build.
import "@afkfleet/ui/styles.css";

import { App } from "@afkfleet/ui";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { TauriApiClient } from "../../src/TauriApiClient";
import { fetchInvoke } from "./fetchInvoke";
import { E2E_MARKER } from "./marker";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("e2e/index.html has no #root element");
}
root.dataset.build = E2E_MARKER;
createRoot(root).render(
  <StrictMode>
    <App apiClient={new TauriApiClient(fetchInvoke())} />
  </StrictMode>,
);
