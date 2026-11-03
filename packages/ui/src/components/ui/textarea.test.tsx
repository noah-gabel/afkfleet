// The textarea copy's one behaviour of its own (ADR-0020, L4): Chromium scrolls only a focused
// textarea's caret into view, which can leave the box, and its focus outline, cut off at a
// scroller's edge, so the whole box is scrolled into view when it takes the focus.
import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { Textarea } from "./textarea";

afterEach(() => {
  vi.restoreAllMocks();
});

describe("Textarea", () => {
  it("scrolls the whole box into view when it takes the focus, and still calls onFocus", () => {
    const scrolled = vi.fn();
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
      configurable: true,
      value: scrolled,
    });
    const onFocus = vi.fn();
    render(<Textarea aria-label="Text" onFocus={onFocus} />);

    screen.getByRole("textbox", { name: "Text" }).focus();

    expect(scrolled).toHaveBeenCalledWith({ block: "nearest", inline: "nearest" });
    expect(onFocus).toHaveBeenCalledTimes(1);
  });
});
