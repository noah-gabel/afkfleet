// The desktop app's frontend entry: it renders the UI from packages/ui. From P8.5 it also
// injects the TauriApiClient.
import "@afkfleet/ui/styles.css";

import { App } from "@afkfleet/ui";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("index.html has no #root element");
}
createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
