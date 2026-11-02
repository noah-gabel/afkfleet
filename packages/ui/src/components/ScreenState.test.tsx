import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ScreenState } from "./ScreenState";

describe("ScreenState", () => {
  it("says loading politely", () => {
    render(<ScreenState kind="loading">Loading…</ScreenState>);

    expect(screen.getByRole("status").textContent).toBe("Loading…");
  });

  it("announces an error at once, in the error's colour", () => {
    render(<ScreenState kind="error">Couldn't load.</ScreenState>);

    const alert = screen.getByRole("alert");
    expect(alert.textContent).toBe("Couldn't load.");
    expect(alert.className).toContain("text-destructive");
  });

  it("leaves an empty list unannounced", () => {
    render(<ScreenState kind="empty">No bots.</ScreenState>);

    expect(screen.getByText("No bots.")).toBeDefined();
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("shows its action", () => {
    render(
      <ScreenState kind="error" action={<button type="button">Load again</button>}>
        Couldn't load.
      </ScreenState>,
    );

    expect(screen.getByRole("button", { name: "Load again" })).toBeDefined();
  });
});
