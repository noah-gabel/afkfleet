// The UI's web entry, for `pnpm dev` here. It has no server connection: every API call
// rejects (UnavailableApiClient). The desktop app has its own entry in apps/desktop, which
// injects the TauriApiClient, and so does its e2e build (apps/desktop/e2e, P8.9).
// `just e2e` builds this entry too and checks it holds nothing of the e2e build.
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { UnavailableApiClient } from "./api/UnavailableApiClient";
import "./styles.css";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("index.html has no #root element");
}
createRoot(root).render(
  <StrictMode>
    <App apiClient={new UnavailableApiClient()} />
  </StrictMode>,
);
