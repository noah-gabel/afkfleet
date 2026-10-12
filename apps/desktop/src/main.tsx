// The desktop app's frontend entry: it renders the UI from packages/ui with the
// TauriApiClient, over `@tauri-apps/api/core`'s `invoke`, the only Tauri API the app uses
// (it needs no core permission; Biome refuses any other import).
import "@afkfleet/ui/styles.css";

import { App } from "@afkfleet/ui";
import { invoke } from "@tauri-apps/api/core";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { TauriApiClient } from "./TauriApiClient";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("index.html has no #root element");
}
createRoot(root).render(
  <StrictMode>
    <App apiClient={new TauriApiClient(invoke)} />
  </StrictMode>,
);
