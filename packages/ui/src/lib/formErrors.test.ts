import { describe, expect, it, vi } from "vitest";

import type { CommandError } from "../generated/CommandError";
import { showFieldErrors } from "./formErrors";

/** A 422 whose fields are `fields`. */
function refused(fields: { path: string; message: string }[]): CommandError {
  return {
    code: "api",
    message: "The server refused the request.",
    api: {
      status: 422,
      code: "validation_failed",
      message: "refused",
      request_id: "01900000-0000-7000-8000-000000000009",
      fields,
    },
  };
}

describe("showFieldErrors", () => {
  it("puts each field's error on the form field it maps to", () => {
    const setError = vi.fn();

    const shown = showFieldErrors(
      setError,
      refused([{ path: "new_password", message: "is the current password" }]),
      { new_password: "password" },
    );

    expect(shown).toBe(true);
    expect(setError).toHaveBeenCalledWith("password", {
      type: "server",
      message: "is the current password",
    });
  });

  it("shows every message for one field, one per line, not only the last", () => {
    const setError = vi.fn();

    showFieldErrors(
      setError,
      refused([
        { path: "new_password", message: "is too weak" },
        { path: "new_password", message: "This is a very common password." },
        { path: "new_password", message: "Add another word or two." },
      ]),
      { new_password: "password" },
    );

    expect(setError).toHaveBeenCalledTimes(1);
    expect(setError).toHaveBeenCalledWith("password", {
      type: "server",
      message: "is too weak\nThis is a very common password.\nAdd another word or two.",
    });
  });

  it("answers false when no field maps to the form", () => {
    const setError = vi.fn();

    const shown = showFieldErrors(setError, refused([{ path: "", message: "must hold one" }]), {
      new_password: "password",
    });

    expect(shown).toBe(false);
    expect(setError).not.toHaveBeenCalled();
  });
});
