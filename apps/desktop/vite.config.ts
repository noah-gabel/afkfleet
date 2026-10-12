// The desktop app's frontend build. `tauri dev` starts this dev server and loads it from
// tauri.conf.json's `devUrl`; `tauri build` embeds `vite build`'s dist/ in the exe.
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  // Keep the Tauri CLI's output visible.
  clearScreen: false,
  server: {
    // Must match `build.devUrl` in src-tauri/tauri.conf.json; fail instead of moving.
    port: 1420,
    strictPort: true,
    // Rust sources aren't part of the frontend; the Tauri CLI rebuilds them itself.
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
});
