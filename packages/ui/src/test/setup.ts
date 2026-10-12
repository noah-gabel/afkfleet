// Vitest setup: Testing Library unmounts what each test rendered, since Vitest runs
// without globals and Testing Library's automatic cleanup needs a global `afterEach`.
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

afterEach(() => {
  cleanup();
});
