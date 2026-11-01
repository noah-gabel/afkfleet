// Light or dark (Plan.md P11.10; ADR-0020, U84, S92): the choice this PC remembers, Windows'
// setting, and the `.dark` class on `<html>` they decide. The CSP allows none of it inline, so
// the bundled script sets the class: themeStart.ts before React renders, `attachTheme` after.
//
// The choice is the one thing the UI keeps in `localStorage`: it isn't a secret, and anything but
// the three values, or a read that fails, counts as Windows. A write that fails still applies the
// choice for this run.
//
// The native window follows the choice too, for its title bar (U85, S94), one call at a time, the
// latest choice after each answer; a refusal is dropped, as the page already shows the choice.
// A choice other than `windows` forces the window's theme and so `prefers-color-scheme` with it,
// which then says nothing about Windows. So the media query counts only under `windows`, and not
// while a forcing call is on its way or was the last to settle, and is read again once a `windows`
// call settled: Windows may have changed while it couldn't be seen.
import type { StoreApi } from "zustand/vanilla";

import type { ApiClient } from "../api/ApiClient";

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
  /** The choice the native window last took, once it answered; none yet this start. */
  readonly sent: ThemeChoice | null;
  /** The choice the native window hasn't answered yet, if any. */
  readonly inFlight: ThemeChoice | null;
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

/** Tells the native window the store's choice, unless it's on its way or already there. */
function sendChoice(store: StoreApi<ThemeState>, api: Pick<ApiClient, "setWindowTheme">): void {
  const { choice, inFlight, sent } = store.getState();
  if (inFlight !== null || sent === choice) {
    return;
  }
  store.setState({ inFlight: choice });
  void api
    .setWindowTheme(choice)
    .catch(() => {})
    .then(() => {
      store.setState({ inFlight: null, sent: choice });
      readWindows(store);
      sendChoice(store, api);
    });
}

/** Whether `call` to the native window forces its theme, and so the media query. */
function forces(call: ThemeChoice | null): boolean {
  return call === "light" || call === "dark";
}

/** Reads Windows' setting into the store, if the media query means it now. */
function readWindows(store: StoreApi<ThemeState>): void {
  const { choice, inFlight, sent, windowsChanged } = store.getState();
  if (choice === "windows" && !forces(inFlight) && !forces(sent)) {
    windowsChanged(windowsTheme());
  }
}

/**
 * Keeps the document, the native window and `store` in step until the returned function is called:
 * the class and the window follow the store, and the store follows Windows' change event.
 */
export function attachTheme(
  store: StoreApi<ThemeState>,
  api: Pick<ApiClient, "setWindowTheme">,
): () => void {
  const onWindowsChange = () => readWindows(store);
  onWindowsChange();
  applyTheme(store.getState().theme);
  sendChoice(store, api);
  const unsubscribe = store.subscribe((state) => {
    applyTheme(state.theme);
    sendChoice(store, api);
  });
  const query = darkQuery();
  query?.addEventListener("change", onWindowsChange);
  return () => {
    unsubscribe();
    query?.removeEventListener("change", onWindowsChange);
  };
}
