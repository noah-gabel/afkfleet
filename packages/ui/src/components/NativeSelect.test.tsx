// What the app's one native select passes through, and what it keeps (ADR-0020, K's "As built").
// jsdom loads no stylesheet, so the e2e sweep checks the colours these classes give.
import { fireEvent, render, screen } from "@testing-library/react";
import { createRef } from "react";
import { describe, expect, it, vi } from "vitest";

import { NativeSelect } from "./NativeSelect";

/** The select, with two options and `props`. */
function rendered(props: Parameters<typeof NativeSelect>[0] = {}) {
  render(
    <NativeSelect aria-label="Pick" {...props}>
      <option value="a">A</option>
      <option value="b">B</option>
    </NativeSelect>,
  );
  return screen.getByRole("combobox", { name: "Pick" }) as HTMLSelectElement;
}

describe("NativeSelect", () => {
  it("passes its props, value, change and options through", () => {
    const onChange = vi.fn();

    const select = rendered({ id: "here", value: "b", disabled: false, onChange });
    fireEvent.change(select, { target: { value: "a" } });

    expect(select.id).toBe("here");
    expect(select.options).toHaveLength(2);
    expect(onChange).toHaveBeenCalledTimes(1);
  });

  it("passes a ref through, as react-hook-form's register needs", () => {
    const ref = createRef<HTMLSelectElement>();

    const select = rendered({ ref });

    expect(ref.current).toBe(select);
  });

  it("holds the tokens for itself and its options", () => {
    const classes = rendered().className.split(" ");

    for (const token of [
      "bg-background",
      "text-foreground",
      "[&_option]:bg-background",
      "[&_option]:text-foreground",
    ]) {
      expect(classes).toContain(token);
    }
  });

  it("keeps its look when a spread brings a className", () => {
    const spread = { className: "bg-transparent" } as Parameters<typeof NativeSelect>[0];

    expect(rendered(spread).className).not.toContain("bg-transparent");
  });
});
