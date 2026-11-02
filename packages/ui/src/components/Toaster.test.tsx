import { act, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

/** The Toaster and sonner imported afresh, over stores of their own. */
async function freshToaster() {
  vi.resetModules();
  const { Toaster } = await import("./Toaster");
  const { toast } = await import("sonner");
  const { StoresProvider } = await import("../app/context");
  const { createAppStores } = await import("../app/stores");
  const stores = createAppStores();
  render(
    <StoresProvider stores={stores}>
      <Toaster />
    </StoresProvider>,
  );
  return {
    toast,
    stores,
    shown: () => document.querySelector<HTMLElement>("[data-sonner-toaster]"),
  };
}

describe("Toaster", () => {
  // sonner 2.0.8 unpatched appends a <style> to <head> when it's imported, which the
  // release CSP blocks. The module registry is reset so this file's import is a fresh one,
  // whatever imported sonner before.
  it("shows a toast without adding a <style> to the document", async () => {
    const { toast, shown } = await freshToaster();

    act(() => {
      toast("Saved.");
    });

    expect(await screen.findByText("Saved.")).toBeDefined();
    expect(document.querySelectorAll("style")).toHaveLength(0);
    expect(shown()).toBeDefined();
    act(() => {
      toast.dismiss();
    });
  });

  // An error or a warning stands out; the rest stay plain (ADR-0020, U91, S116).
  it.each([
    ["error", "--destructive"],
    ["warning", "--warning"],
  ] as const)("tints a toast of type %s from its token", async (type, token) => {
    const { toast } = await freshToaster();

    act(() => {
      toast[type]("Look.");
    });

    const shown = (await screen.findByText("Look.")).closest<HTMLElement>("[data-sonner-toast]");
    expect(shown?.className).toContain(`[--normal-bg:color-mix(in_srgb,var(${token})_12%`);
    expect(shown?.className).toContain(`[--normal-border:var(${token})]`);
    act(() => {
      toast.dismiss();
    });
  });

  it.each(["success", "info"] as const)("leaves a toast of type %s plain", async (type) => {
    const { toast } = await freshToaster();

    act(() => {
      toast[type]("Fine.");
    });

    const shown = (await screen.findByText("Fine.")).closest<HTMLElement>("[data-sonner-toast]");
    expect(shown?.className).not.toContain("--normal-bg");
    act(() => {
      toast.dismiss();
    });
  });

  it("leaves a toast of no type plain, in the app's font", async () => {
    const { toast, shown } = await freshToaster();

    act(() => {
      toast("Fine.");
    });

    const held = (await screen.findByText("Fine.")).closest<HTMLElement>("[data-sonner-toast]");
    expect(held?.className).not.toContain("--normal-bg");
    expect(shown()?.className).toContain("font-sans");
    act(() => {
      toast.dismiss();
    });
  });

  // sonner reads its theme from our store, not next-themes (ADR-0020, U84).
  it("follows the app's theme, live", async () => {
    const { toast, shown, stores } = await freshToaster();
    act(() => {
      toast("Saved.");
    });
    await screen.findByText("Saved.");
    expect(shown()?.dataset.sonnerTheme).toBe("light");

    act(() => stores.theme.getState().choose("dark"));

    expect(shown()?.dataset.sonnerTheme).toBe("dark");
    act(() => {
      toast.dismiss();
    });
  });
});
