// The e2e build's entry (Plan.md P8.9, ADR-0017): the desktop app's UI with its real
// TauriApiClient, over fetchInvoke instead of Tauri's IPC, so Playwright drives what ships,
// minus Tauri. Its live events channel is a placeholder: the fake holds the watch and sends
// no update. Only vite.e2e.config.ts builds it, into dist-e2e/, never the app's own build.
import "@afkfleet/ui/styles.css";

import { App } from "@afkfleet/ui";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { type OpenChannel, TauriApiClient } from "../../src/TauriApiClient";
import { fetchInvoke } from "./fetchInvoke";
import { E2E_MARKER } from "./marker";

/** A channel the fake never sends on: the value a real one serializes to. */
const openChannel: OpenChannel = () => "__CHANNEL__:0";

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
