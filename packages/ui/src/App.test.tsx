import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { App } from "./App";

describe("App", () => {
  it("shows the app's name as its main heading", () => {
    render(<App />);

    expect(screen.getByRole("heading", { level: 1, name: "afkfleet" })).toBeDefined();
  });
});
