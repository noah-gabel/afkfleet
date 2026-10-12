// Vitest for the desktop app's frontend entry: TauriApiClient's tests run in Node, with a
// fake `invoke`; nothing here needs a DOM.
import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
