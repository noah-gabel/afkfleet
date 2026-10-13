import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { FieldMessage } from "./FieldMessage";
import { PasswordInput } from "./PasswordInput";

describe("FieldMessage", () => {
  it("shows the message under the id its input points to", () => {
    render(
      <>
        <input aria-label="Username" aria-describedby="username-message" aria-invalid />
        <FieldMessage id="username-message" message="must be 3 to 32 letters" />
      </>,
    );

    const input = screen.getByLabelText("Username");
    const id = input.getAttribute("aria-describedby") ?? "";
    expect(document.getElementById(id)?.textContent).toBe("must be 3 to 32 letters");
  });

  it("renders nothing without a message", () => {
    const { container } = render(<FieldMessage id="x" message={undefined} />);

    expect(container.childElementCount).toBe(0);
  });
});

describe("PasswordInput", () => {
  it("hides the password until its toggle is pressed, then hides it again", async () => {
    const user = userEvent.setup();
    render(<PasswordInput aria-label="Password" toggleLabel="Show password" />);
    const input = screen.getByLabelText("Password");
    const toggle = screen.getByRole("button", { name: "Show password" });

    const hidden = [input.getAttribute("type"), toggle.getAttribute("aria-pressed")];
    await user.click(toggle);
    const shown = [input.getAttribute("type"), toggle.getAttribute("aria-pressed")];
    await user.click(toggle);

    expect(hidden).toEqual(["password", "false"]);
    expect(shown).toEqual(["text", "true"]);
    expect(input.getAttribute("type")).toBe("password");
  });

  it("is a plain button, so it never submits its form", () => {
    render(<PasswordInput aria-label="Password" toggleLabel="Show password" />);

    expect(screen.getByRole("button", { name: "Show password" }).getAttribute("type")).toBe(
      "button",
    );
  });
});
