// The desktop app's frontend entry: it renders the UI from packages/ui with the
// TauriApiClient, over `@tauri-apps/api/core`'s `invoke` and its `Channel` for the live
// events, the only Tauri APIs the app uses (they need no core permission; Biome refuses any
// other import).
import "@afkfleet/ui/styles.css";

import { App } from "@afkfleet/ui";
import { Channel, invoke } from "@tauri-apps/api/core";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { type OpenChannel, TauriApiClient } from "./TauriApiClient";

/** A Tauri channel whose messages go to `onMessage`. */
const openChannel: OpenChannel = (onMessage) => new Channel(onMessage);

const root = document.getElementById("root");
if (root === null) {
  throw new Error("index.html has no #root element");
}
createRoot(root).render(
  <StrictMode>
    <App apiClient={new TauriApiClient(invoke, openChannel)} />
  </StrictMode>,
);
