// A stand-in for the window's `matchMedia`, which jsdom doesn't have: the theme's tests say what
// Windows' setting is, and change it (Plan.md P11.10). setup.ts unstubs it after each test.
import { vi } from "vitest";

/** What a test holds of the fake. */
export interface FakeMedia {
  /** Windows' setting changes: the query answers it, then its listeners hear. */
  change(dark: boolean): void;
  /** How many listeners are attached now. */
  listeners(): number;
}

/** Stubs `matchMedia` for `prefers-color-scheme: dark`, answering `dark`. */
export function fakeMatchMedia(dark: boolean): FakeMedia {
  const heard = new Set<() => void>();
  const media = {
    matches: dark,
    addEventListener(_type: string, listener: () => void) {
      heard.add(listener);
    },
    removeEventListener(_type: string, listener: () => void) {
      heard.delete(listener);
    },
  };
  vi.stubGlobal("matchMedia", (query: string) => {
    if (query !== "(prefers-color-scheme: dark)") {
      throw new Error(`nothing asks matchMedia for ${query}`);
    }
    return media;
  });
  return {
    change(next) {
      media.matches = next;
      for (const listener of heard) {
        listener();
      }
    },
    listeners: () => heard.size,
  };
}
