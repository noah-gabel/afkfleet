// The UI's own Vite config: its web entry (index.html) for development, and Vitest. The
// desktop app bundles the UI through its own configs in apps/desktop: the app's, and the
// e2e build's (P8.9).
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  // Nothing inlined as `data:`: the CSP refuses a font that way (ADR-0020, S111).
  build: { assetsInlineLimit: 0 },
  server: {
    port: 5173,
    strictPort: true,
  },
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
  },
});
