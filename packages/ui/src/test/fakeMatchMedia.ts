// A stand-in for the window's `matchMedia`, which jsdom doesn't have: the theme's tests say what
// Windows' setting is, and change it (Plan.md P11.10). setup.ts unstubs it after each test.
import { vi } from "vitest";

/** What a test holds of the fake. */
export interface FakeMedia {
  /**
   * Windows' setting changes: the query answers it, then its listeners hear. Under a force the
   * query stays as it is and they hear nothing, as in the webview.
   */
  change(dark: boolean): void;
  /**
   * The native window forces its theme on the webview, or with `null` stops: the query answers the
   * forced theme, or Windows' again, and if that moved it, its listeners hear.
   */
  force(theme: "light" | "dark" | null): void;
  /** How many listeners are attached now. */
  listeners(): number;
}

/** Stubs `matchMedia` for `prefers-color-scheme: dark`, answering `dark`. */
export function fakeMatchMedia(dark: boolean): FakeMedia {
  const heard = new Set<() => void>();
  let windows = dark;
  let forced: "light" | "dark" | null = null;
  const media = {
    matches: dark,
    addEventListener(_type: string, listener: () => void) {
      heard.add(listener);
    },
    removeEventListener(_type: string, listener: () => void) {
      heard.delete(listener);
    },
  };
  const tell = () => {
    for (const listener of heard) {
      listener();
    }
  };
  vi.stubGlobal("matchMedia", (query: string) => {
    if (query !== "(prefers-color-scheme: dark)") {
      throw new Error(`nothing asks matchMedia for ${query}`);
    }
    return media;
  });
  return {
    change(next) {
      windows = next;
      if (forced === null) {
        media.matches = next;
        tell();
      }
    },
    force(theme) {
      forced = theme;
      const answer = theme === null ? windows : theme === "dark";
      if (answer !== media.matches) {
        media.matches = answer;
        tell();
      }
    },
    listeners: () => heard.size,
  };
}
