import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { Tooltip } from "./Tooltip";

function sample() {
  render(
    <>
      <button type="button">Before</button>
      <Tooltip label="Log out">
        <button type="button" aria-label="Log out">
          <svg aria-hidden />
        </button>
      </Tooltip>
    </>,
  );
  return screen.getByRole("button", { name: "Log out" });
}

const tip = () => document.querySelector<HTMLElement>("[data-slot=tooltip]");

describe("Tooltip", () => {
  it("shows nothing until it's asked", () => {
    sample();

    expect(tip()).toBeNull();
  });

  it("opens on hover and closes when the pointer leaves", async () => {
    const user = userEvent.setup();
    const button = sample();

    await user.hover(button);
    expect(tip()?.textContent).toBe("Log out");

    await user.unhover(button);
    expect(tip()).toBeNull();
  });

  it("opens when Tab reaches it and closes when Tab leaves", async () => {
    const user = userEvent.setup();
    sample();

    await user.tab();
    await user.tab();
    expect(tip()?.textContent).toBe("Log out");

    await user.tab();
    expect(tip()).toBeNull();
  });

  it("doesn't open from a click's focus, and a click closes it", async () => {
    const user = userEvent.setup();
    const button = sample();

    await user.hover(button);
    await user.click(button);
    expect(tip()).toBeNull();

    // Focus stays on the button; the tip isn't what that focus asked for.
    expect(document.activeElement).toBe(button);
  });

  it("closes on Escape, wherever the focus is", async () => {
    const user = userEvent.setup();
    const button = sample();

    await user.hover(button);
    expect(tip()).not.toBeNull();
    await user.keyboard("{Escape}");
    expect(tip()).toBeNull();

    await user.tab();
    await user.tab();
    expect(tip()).not.toBeNull();
    await user.keyboard("{Escape}");
    expect(tip()).toBeNull();
    expect(document.activeElement).toBe(button);
  });

  it("stays open while the pointer is on the tip itself", async () => {
    const user = userEvent.setup();
    const button = sample();

    await user.hover(button);
    const held = tip();
    expect(held).not.toBeNull();
    await act(async () => {
      await user.pointer({ target: held as HTMLElement });
    });

    expect(tip()).not.toBeNull();
  });

  it("is seen, not read: the name comes from the control, once", async () => {
    const user = userEvent.setup();
    const button = sample();

    await user.hover(button);

    expect(tip()?.getAttribute("aria-hidden")).toBe("true");
    expect(tip()?.getAttribute("role")).toBeNull();
    expect(button.getAttribute("aria-describedby")).toBeNull();
    expect(button.getAttribute("title")).toBeNull();
    expect(screen.getAllByRole("button", { name: "Log out" })).toHaveLength(1);
  });
});
