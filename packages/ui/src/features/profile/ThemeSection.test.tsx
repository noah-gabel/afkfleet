// The Profile screen's theme setting (Plan.md P11.10; ADR-0020, U84): Light, Dark or Windows, the
// choice shown at once and remembered on this PC.
import { screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { THEME_KEY } from "../../app/theme";
import { fakeMatchMedia } from "../../test/fakeMatchMedia";
import { atScreen } from "../../test/signedIn";

/** The setting's group. */
async function setting() {
  return within(await screen.findByRole("group", { name: "Theme" }));
}

describe("the theme setting", () => {
  it("offers the three choices under Appearance, Windows by default", async () => {
    await atScreen("/profile");

    expect(await screen.findByRole("heading", { name: "Appearance", level: 2 })).toBeDefined();
    const theme = await setting();
    const radio = (name: string) => theme.getByRole("radio", { name }) as HTMLInputElement;
    expect(radio("Light").checked).toBe(false);
    expect(radio("Dark").checked).toBe(false);
    expect(radio("Windows").checked).toBe(true);
    expect(
      theme.getByRole("radio", { name: "Windows", description: "Follows your Windows setting." }),
    ).toBeDefined();
  });

  it("shows the remembered choice", async () => {
    localStorage.setItem(THEME_KEY, "dark");

    await atScreen("/profile");

    const theme = await setting();
    expect((theme.getByRole("radio", { name: "Dark" }) as HTMLInputElement).checked).toBe(true);
  });

  it("applies a choice at once, remembers it, and can go back to following Windows", async () => {
    fakeMatchMedia(false);
    const app = await atScreen("/profile");
    const theme = await setting();
    const dark = document.documentElement.classList;

    await app.user.click(theme.getByRole("radio", { name: "Dark" }));

    expect(dark.contains("dark")).toBe(true);
    expect(localStorage.getItem(THEME_KEY)).toBe("dark");

    await app.user.click(theme.getByRole("radio", { name: "Windows" }));

    expect(dark.contains("dark")).toBe(false);
    expect(localStorage.getItem(THEME_KEY)).toBe("windows");
  });
});
