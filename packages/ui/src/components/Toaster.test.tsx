import { act, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

describe("Toaster", () => {
  // sonner 2.0.8 unpatched appends a <style> to <head> when it's imported, which the
  // release CSP blocks. The module registry is reset so this file's import is a fresh one,
  // whatever imported sonner before.
  it("shows a toast without adding a <style> to the document", async () => {
    vi.resetModules();
    const { Toaster } = await import("./Toaster");
    const { toast } = await import("sonner");

    render(<Toaster />);
    act(() => {
      toast("Saved.");
    });

    expect(await screen.findByText("Saved.")).toBeDefined();
    expect(document.querySelectorAll("style")).toHaveLength(0);
    act(() => {
      toast.dismiss();
    });
  });
});
