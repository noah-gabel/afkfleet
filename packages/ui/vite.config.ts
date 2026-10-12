// The UI's own Vite config: its web entry (index.html) for development and, from P8.9,
// the Playwright build; and Vitest. The desktop app bundles the UI through its own config
// in apps/desktop.
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    port: 5173,
    strictPort: true,
  },
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
  },
});
