import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Logo, Wordmark } from "./Brand";

describe("Wordmark", () => {
  it("reads as one word, and shows two weights", () => {
    render(
      <h1>
        <Wordmark />
      </h1>,
    );

    expect(screen.getByRole("heading", { name: "afkfleet" })).toBeDefined();
    expect(screen.getByText("afkfleet")).toBeDefined();
    const shown = document.querySelector("[data-slot=wordmark]");
    expect(shown?.getAttribute("aria-hidden")).toBe("true");
    expect(shown?.querySelector(".font-bold")?.textContent).toBe("fleet");
  });
});

describe("Logo", () => {
  it("is decorative, at its size", () => {
    render(<Logo size={20} />);

    const image = document.querySelector("img");
    expect(image?.getAttribute("alt")).toBe("");
    expect(image?.getAttribute("width")).toBe("20");
    expect(screen.queryByRole("img")).toBeNull();
  });
});
