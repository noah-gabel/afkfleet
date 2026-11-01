// Light or dark (Plan.md P11.10; ADR-0020, U84, S92): the choice this PC remembers, Windows'
// setting, and the `.dark` class on `<html>` they decide. The CSP allows none of it inline, so
// the bundled script sets the class: themeStart.ts before React renders, `attachTheme` after.
//
// The choice is the one thing the UI keeps in `localStorage`: it isn't a secret, and anything but
// the three values, or a read that fails, counts as Windows. A write that fails still applies the
// choice for this run.
import type { StoreApi } from "zustand/vanilla";

/** What a user may choose. */
export type ThemeChoice = "light" | "dark" | "windows";

/** What the app shows. */
export type Theme = "light" | "dark";

/** Where this PC remembers the choice. */
export const THEME_KEY = "afkfleet.theme";

/** The theme store; see the module comment. */
export interface ThemeState {
  readonly choice: ThemeChoice;
  /** What Windows says, as far as the app can tell. */
  readonly windows: Theme;
  /** What the app shows: the choice, or under `windows`, what Windows says. */
  readonly theme: Theme;
  /** The user chose: remembered, and shown at once. */
  choose(choice: ThemeChoice): void;
  /** Windows' setting changed. */
  windowsChanged(windows: Theme): void;
}

/** The query Windows' setting answers. */
const DARK_QUERY = "(prefers-color-scheme: dark)";

/** The choice this PC remembers; see the module comment. */
export function readThemeChoice(): ThemeChoice {
  try {
    const stored = localStorage.getItem(THEME_KEY);
    return stored === "light" || stored === "dark" ? stored : "windows";
  } catch {
    return "windows";
  }
}

/** Remembers `choice`, if this PC lets it. */
export function saveThemeChoice(choice: ThemeChoice): void {
  try {
    localStorage.setItem(THEME_KEY, choice);
  } catch {
    // It applies for this run all the same; the next start follows what's remembered.
  }
}

/** The media query behind Windows' setting, where the webview has one. */
function darkQuery(): MediaQueryList | null {
  return typeof globalThis.matchMedia === "function" ? globalThis.matchMedia(DARK_QUERY) : null;
}

/** What Windows says; light where the webview can't tell. */
export function windowsTheme(): Theme {
  return darkQuery()?.matches === true ? "dark" : "light";
}

/** What `choice` shows while Windows says `windows`. */
export function resolveTheme(choice: ThemeChoice, windows: Theme): Theme {
  return choice === "windows" ? windows : choice;
}

/** Shows `theme`: the class on `<html>`, which the stylesheet's tokens follow. */
export function applyTheme(theme: Theme): void {
  const classes = document.documentElement.classList;
  classes.toggle("dark", theme === "dark");
  classes.toggle("light", theme === "light");
}

/**
 * Keeps the document and `store` in step until the returned function is called: the class follows
 * the store, and the store follows Windows' change event.
 */
export function attachTheme(store: StoreApi<ThemeState>): () => void {
  const onWindowsChange = () => store.getState().windowsChanged(windowsTheme());
  onWindowsChange();
  applyTheme(store.getState().theme);
  const unsubscribe = store.subscribe((state) => applyTheme(state.theme));
  const query = darkQuery();
  query?.addEventListener("change", onWindowsChange);
  return () => {
    unsubscribe();
    query?.removeEventListener("change", onWindowsChange);
  };
}
