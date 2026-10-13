import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { App } from "./App";
import { UnavailableApiClient } from "./api/UnavailableApiClient";

describe("App", () => {
  it("names the app and says why the web build can't start", async () => {
    render(<App apiClient={new UnavailableApiClient()} />);

    expect(await screen.findByRole("heading", { level: 1, name: "afkfleet" })).toBeDefined();
    expect(screen.getByText("The web build has no server connection.")).toBeDefined();
  });
});
