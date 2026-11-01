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

/** A native window that takes every choice at once, for the tests about the class. */
function quiet() {
  return { setWindowTheme: async () => {} };
}

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
    const release = attachTheme(store, quiet());
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
    const release = attachTheme(store, quiet());
    expect(classes()).toEqual(shown);

    windows.change(!dark);

    expect(classes()).toEqual(shown);
    release();
  });

  it("follows the store when the user chooses", () => {
    fakeMatchMedia(false);
    const store = createAppStores().theme;
    const release = attachTheme(store, quiet());

    store.getState().choose("dark");

    expect(classes()).toEqual(["dark"]);
    release();
  });

  it("re-reads Windows when it attaches, for a change while it wasn't listening", () => {
    const windows = fakeMatchMedia(false);
    const store = createAppStores().theme;
    windows.change(true);

    const release = attachTheme(store, quiet());

    expect(store.getState().windows).toBe("dark");
    expect(classes()).toEqual(["dark"]);
    release();
  });

  it("lets go of both when released, and leaves the class", () => {
    const windows = fakeMatchMedia(true);
    const store = createAppStores().theme;
    const release = attachTheme(store, quiet());
    expect(windows.listeners()).toBe(1);

    release();

    expect(windows.listeners()).toBe(0);
    store.getState().choose("light");
    expect(classes()).toEqual(["dark"]);
  });

  it("attaches twice as StrictMode does, leaving one listener", () => {
    const windows = fakeMatchMedia(true);
    const store = createAppStores().theme;

    attachTheme(store, quiet())();
    const release = attachTheme(store, quiet());

    expect(windows.listeners()).toBe(1);
    release();
  });

  it("works without matchMedia", () => {
    const store = createAppStores().theme;

    const release = attachTheme(store, quiet());

    expect(classes()).toEqual(["light"]);
    expect(release).not.toThrow();
  });
});

describe("the native window (ADR-0020, S94)", () => {
  /** A native window whose calls wait until a test answers them. */
  function heldWindow() {
    const pending: { choice: string; answer: () => void; refuse: () => void }[] = [];
    return {
      pending,
      setWindowTheme: (choice: string) =>
        new Promise<void>((resolve, reject) => {
          pending.push({ choice, answer: resolve, refuse: () => reject(new Error("no window")) });
        }),
    };
  }

  /** Lets what a settled call does next run. */
  const settled = () => new Promise((resolve) => setTimeout(resolve));

  it("takes the choice when the App attaches, Windows too, which ends a force a reload left", () => {
    fakeMatchMedia(false);
    const native = heldWindow();

    const release = attachTheme(createAppStores().theme, native);

    expect(native.pending.map((call) => call.choice)).toEqual(["windows"]);
    release();
  });

  it("sends once when attached twice, as StrictMode does", () => {
    fakeMatchMedia(false);
    const native = heldWindow();
    const store = createAppStores().theme;

    attachTheme(store, native)();
    const release = attachTheme(store, native);

    expect(native.pending).toHaveLength(1);
    release();
  });

  it("sends a choice once the last call answered, and only the latest of many", async () => {
    fakeMatchMedia(false);
    const native = heldWindow();
    const store = createAppStores().theme;
    const release = attachTheme(store, native);

    store.getState().choose("light");
    store.getState().choose("windows");
    store.getState().choose("dark");
    expect(native.pending.map((call) => call.choice)).toEqual(["windows"]);
    native.pending[0]?.answer();
    await settled();

    expect(native.pending.map((call) => call.choice)).toEqual(["windows", "dark"]);
    native.pending[1]?.answer();
    await settled();
    expect(native.pending).toHaveLength(2);
    expect(store.getState().sent).toBe("dark");
    release();
  });

  it("shows the choice before the native window answers", () => {
    fakeMatchMedia(false);
    const native = heldWindow();
    const store = createAppStores().theme;
    const release = attachTheme(store, native);

    store.getState().choose("dark");

    expect(classes()).toEqual(["dark"]);
    release();
  });

  it("keeps the theme and raises nothing when the native window refuses, then goes on", async () => {
    fakeMatchMedia(false);
    const native = heldWindow();
    const store = createAppStores().theme;
    const release = attachTheme(store, native);
    store.getState().choose("dark");

    native.pending[0]?.refuse();
    await settled();

    expect(classes()).toEqual(["dark"]);
    expect(native.pending.map((call) => call.choice)).toEqual(["windows", "dark"]);
    release();
  });

  it("ignores what the media query says under a manual choice: the window forces it", () => {
    const windows = fakeMatchMedia(true);
    const store = createAppStores().theme;
    store.getState().choose("light");
    const release = attachTheme(store, quiet());

    windows.change(false);
    windows.change(true);
    windows.change(false);

    expect(store.getState().windows).toBe("dark");
    expect(classes()).toEqual(["light"]);
    release();
  });

  it("reads Windows again once the window stopped forcing, for a change it couldn't see", async () => {
    const windows = fakeMatchMedia(false);
    const native = heldWindow();
    const store = createAppStores().theme;
    store.getState().choose("dark");
    const release = attachTheme(store, native);
    native.pending[0]?.answer();
    await settled();
    // Windows went dark under the forced theme, so nothing fired.
    windows.change(true);
    expect(store.getState().windows).toBe("light");

    store.getState().choose("windows");
    expect(native.pending.map((call) => call.choice)).toEqual(["dark", "windows"]);
    native.pending[1]?.answer();
    await settled();

    expect(store.getState().windows).toBe("dark");
    expect(classes()).toEqual(["dark"]);
    release();
  });

  it("follows Windows' change while the Windows call is still on its way", () => {
    const windows = fakeMatchMedia(false);
    const native = heldWindow();
    const store = createAppStores().theme;
    const release = attachTheme(store, native);

    windows.change(true);

    expect(classes()).toEqual(["dark"]);
    release();
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
