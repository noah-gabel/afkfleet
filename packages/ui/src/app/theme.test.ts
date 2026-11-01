// The theme's state and class (Plan.md P11.10; ADR-0020, U84, S92): what this PC remembers, what
// Windows says, and what `<html>` shows of them, live.
import { describe, expect, it, vi } from "vitest";

import { fakeMatchMedia } from "../test/fakeMatchMedia";
import { createAppStores } from "./stores";
import {
  applyTheme,
  attachTheme,
  readThemeChoice,
  resolveTheme,
  saveThemeChoice,
  THEME_KEY,
  type ThemeChoice,
  windowsTheme,
} from "./theme";

/** What `<html>` holds of the two theme classes. */
function classes(): string[] {
  const list = document.documentElement.classList;
  return ["light", "dark"].filter((name) => list.contains(name));
}

describe("the remembered choice", () => {
  it("is Windows when nothing is remembered", () => {
    expect(readThemeChoice()).toBe("windows");
  });

  it.each(["light", "dark", "windows"] as const)("reads back %s", (choice) => {
    saveThemeChoice(choice);

    expect(localStorage.getItem(THEME_KEY)).toBe(choice);
    expect(readThemeChoice()).toBe(choice);
  });

  it.each(["", "Dark", "system", "__proto__", "dark "])("counts %j as Windows", (stored) => {
    localStorage.setItem(THEME_KEY, stored);

    expect(readThemeChoice()).toBe("windows");
  });

  it("counts a read that throws as Windows", () => {
    vi.stubGlobal("localStorage", {
      getItem() {
        throw new Error("blocked");
      },
    });

    expect(readThemeChoice()).toBe("windows");
  });

  it("survives a write that throws", () => {
    vi.stubGlobal("localStorage", {
      setItem() {
        throw new Error("full");
      },
    });

    expect(() => saveThemeChoice("dark")).not.toThrow();
  });
});

describe("what Windows says", () => {
  it.each([
    [true, "dark"],
    [false, "light"],
  ] as const)("reads the media query's %s as %s", (dark, theme) => {
    fakeMatchMedia(dark);

    expect(windowsTheme()).toBe(theme);
  });

  it("is light where the webview has no matchMedia", () => {
    expect(windowsTheme()).toBe("light");
  });
});

describe("resolveTheme", () => {
  it.each([
    ["light", "dark", "light"],
    ["light", "light", "light"],
    ["dark", "light", "dark"],
    ["dark", "dark", "dark"],
    ["windows", "dark", "dark"],
    ["windows", "light", "light"],
  ] as const)("shows %s while Windows says %s as %s", (choice, windows, theme) => {
    expect(resolveTheme(choice, windows)).toBe(theme);
  });
});

describe("applyTheme", () => {
  it("holds exactly one of the two classes", () => {
    applyTheme("dark");
    expect(classes()).toEqual(["dark"]);

    applyTheme("light");
    expect(classes()).toEqual(["light"]);
  });
});

describe("the store", () => {
  it("starts from what's remembered and what Windows says", () => {
    fakeMatchMedia(true);
    saveThemeChoice("light");

    const state = createAppStores().theme.getState();

    expect(state.choice).toBe("light");
    expect(state.windows).toBe("dark");
    expect(state.theme).toBe("light");
  });

  it("remembers a choice and shows it at once", () => {
    const store = createAppStores().theme;

    store.getState().choose("dark");

    expect(store.getState().theme).toBe("dark");
    expect(localStorage.getItem(THEME_KEY)).toBe("dark");
  });

  it("still applies a choice this PC can't remember", () => {
    const store = createAppStores().theme;
    vi.stubGlobal("localStorage", {
      setItem() {
        throw new Error("full");
      },
    });

    store.getState().choose("dark");

    expect(store.getState().theme).toBe("dark");
  });
});

describe("attachTheme", () => {
  it("shows Windows' setting at once and follows its change, live", () => {
    const windows = fakeMatchMedia(true);
    const store = createAppStores().theme;
    const release = attachTheme(store);
    expect(classes()).toEqual(["dark"]);

    windows.change(false);
    expect(classes()).toEqual(["light"]);
    expect(store.getState().theme).toBe("light");

    windows.change(true);
    expect(classes()).toEqual(["dark"]);
    release();
  });

  it.each([
    ["light", true, ["light"]],
    ["dark", false, ["dark"]],
  ] as const)("keeps a choice of %s whatever Windows says", (choice, dark, shown) => {
    const windows = fakeMatchMedia(dark);
    const store = createAppStores().theme;
    store.getState().choose(choice as ThemeChoice);
    const release = attachTheme(store);
    expect(classes()).toEqual(shown);

    windows.change(!dark);

    expect(classes()).toEqual(shown);
    release();
  });

  it("follows the store when the user chooses", () => {
    fakeMatchMedia(false);
    const store = createAppStores().theme;
    const release = attachTheme(store);

    store.getState().choose("dark");

    expect(classes()).toEqual(["dark"]);
    release();
  });

  it("re-reads Windows when it attaches, for a change while it wasn't listening", () => {
    const windows = fakeMatchMedia(false);
    const store = createAppStores().theme;
    windows.change(true);

    const release = attachTheme(store);

    expect(store.getState().windows).toBe("dark");
    expect(classes()).toEqual(["dark"]);
    release();
  });

  it("lets go of both when released, and leaves the class", () => {
    const windows = fakeMatchMedia(true);
    const store = createAppStores().theme;
    const release = attachTheme(store);
    expect(windows.listeners()).toBe(1);

    release();

    expect(windows.listeners()).toBe(0);
    store.getState().choose("light");
    expect(classes()).toEqual(["dark"]);
  });

  it("attaches twice as StrictMode does, leaving one listener", () => {
    const windows = fakeMatchMedia(true);
    const store = createAppStores().theme;

    attachTheme(store)();
    const release = attachTheme(store);

    expect(windows.listeners()).toBe(1);
    release();
  });

  it("works without matchMedia", () => {
    const store = createAppStores().theme;

    const release = attachTheme(store);

    expect(classes()).toEqual(["light"]);
    expect(release).not.toThrow();
  });
});

describe("themeStart", () => {
  /** Runs the module afresh, as an entry's first import does. */
  async function start() {
    vi.resetModules();
    await import("./themeStart");
  }

  it("sets the class before anything renders, from what's remembered", async () => {
    fakeMatchMedia(false);
    localStorage.setItem(THEME_KEY, "dark");

    await start();

    expect(classes()).toEqual(["dark"]);
  });

  it("sets Windows' with nothing remembered", async () => {
    fakeMatchMedia(true);

    await start();

    expect(classes()).toEqual(["dark"]);
  });

  it("sets light over a dark Windows, so the stylesheet's first guess gives way", async () => {
    fakeMatchMedia(true);
    localStorage.setItem(THEME_KEY, "light");

    await start();

    expect(classes()).toEqual(["light"]);
  });
});
