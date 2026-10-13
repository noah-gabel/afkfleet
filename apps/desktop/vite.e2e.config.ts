// The e2e build (Plan.md P8.9, ADR-0017): e2e/index.html, the desktop app's UI over
// fetchInvoke, into dist-e2e/, never the app's own dist/. `vite preview` serves it for
// Playwright with the release CSP of src-tauri/tauri.conf.json, only `connect-src` changed to
// `'self'` for fetchInvoke (e2e/csp.ts).
import { readFileSync } from "node:fs";

import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

import { HOST, PORT } from "./e2e/base.ts";
import { e2eCsp } from "./e2e/csp.ts";

const tauri: { app: { security: { csp: string } } } = JSON.parse(
  readFileSync(new URL("./src-tauri/tauri.conf.json", import.meta.url), "utf8"),
);

export default defineConfig({
  root: "e2e",
  plugins: [react(), tailwindcss()],
  build: {
    outDir: "../dist-e2e",
    emptyOutDir: true,
  },
  preview: {
    host: HOST,
    port: PORT,
    strictPort: true,
    headers: { "Content-Security-Policy": e2eCsp(tauri.app.security.csp) },
  },
});
