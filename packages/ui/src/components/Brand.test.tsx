import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import ICON from "../../../../apps/desktop/src-tauri/icons/icon.svg?raw";
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
  const drawn = () => {
    render(<Logo size={20} />);
    const logo = document.querySelector("svg");
    if (logo === null) {
      throw new Error("no inline SVG");
    }
    return logo;
  };
  const attributes = (element: Element | null | undefined, names: readonly string[]) =>
    names.map((name) => element?.getAttribute(name));

  it("is decorative, at its size", () => {
    const logo = drawn();

    expect(logo.getAttribute("aria-hidden")).toBe("true");
    expect(logo.getAttribute("width")).toBe("20");
    expect(logo.getAttribute("height")).toBe("20");
    expect(screen.queryByRole("img")).toBeNull();
  });

  // The Windows icon keeps its tile; the app's logo is only the marks (ADR-0020, U104).
  it("has no tile and no image", () => {
    const logo = drawn();

    expect(logo.querySelector("rect")).toBeNull();
    expect(document.querySelector("img")).toBeNull();
  });

  it("draws the Windows icon's ring and both circles, exactly", () => {
    const logo = drawn();
    const icon = new DOMParser().parseFromString(ICON, "image/svg+xml");
    const [dot, satellite] = icon.querySelectorAll("circle");
    const [ownDot, ownSatellite] = logo.querySelectorAll(":scope > circle");
    const RING = ["d", "stroke-width", "stroke-linecap", "fill"];
    const CIRCLE = ["cx", "cy", "r"];

    expect(icon.querySelectorAll("circle")).toHaveLength(2);
    expect(attributes(logo.querySelector(":scope > path"), RING)).toEqual(
      attributes(icon.querySelector("path"), RING),
    );
    expect(attributes(ownDot, CIRCLE)).toEqual(attributes(dot, CIRCLE));
    expect(attributes(ownSatellite, CIRCLE)).toEqual(attributes(satellite, CIRCLE));
  });

  it("takes its colours from the theme's tokens, the satellite strongest and on top", () => {
    const logo = drawn();
    const parts = [...logo.querySelectorAll(":scope > path, :scope > circle")];

    expect(parts.map((part) => part.getAttribute("class"))).toEqual([
      "stroke-primary",
      "fill-primary",
      "fill-foreground",
    ]);
    expect(logo.querySelector(":scope > path")?.getAttribute("stroke")).toBeNull();
    for (const part of logo.querySelectorAll(":scope > circle")) {
      expect(part.getAttribute("fill")).toBeNull();
    }
  });

  // Tokens alone can't part the satellite from the ring it overlaps: 2.75:1 in light, 1.67 in dark.
  it("clears a gap in the ring around the satellite, leaving the ring's path alone", () => {
    const logo = drawn();
    const clip = logo.querySelector("clipPath");
    const hole = clip?.querySelector("path");
    const satellite = logo.querySelectorAll(":scope > circle")[1];
    const [cx, cy, r] = attributes(satellite, ["cx", "cy", "r"]).map(Number) as [
      number,
      number,
      number,
    ];

    expect(logo.querySelector(":scope > path")?.getAttribute("clip-path")).toBe(
      `url(#${clip?.getAttribute("id")})`,
    );
    expect(hole?.getAttribute("clip-rule")).toBe("evenodd");
    const edge = r + 32;
    expect(hole?.getAttribute("d")).toContain(`M ${cx - edge} ${cy} A ${edge} ${edge} 0 1 0`);
  });
});
