// The window's focus, for TanStack Query (Plan.md P9.8, S5 in ADR-0018's group F). TanStack
// Query 5 sees focus only through `visibilitychange`, which misses a click back to the app's
// window when it stayed visible behind another one. While an App is mounted, the window's own
// `focus` and `blur` count too. Only queries that ask for it (`refetchOnWindowFocus`, the
// links waiting for approval) read again; every other query keeps the app's `false`.
import { focusManager } from "@tanstack/react-query";
import { useEffect } from "react";

type SetFocused = (focused?: boolean) => void;

/** TanStack Query's own listener: the page's visibility. */
function visibility(setFocused: SetFocused): () => void {
  const onChange = () => setFocused();
  window.addEventListener("visibilitychange", onChange, false);
  return () => window.removeEventListener("visibilitychange", onChange);
}

/** The page's visibility, and the window's focus and blur. */
function windowFocus(setFocused: SetFocused): () => void {
  const onVisibility = () => setFocused();
  const onFocus = () => setFocused(true);
  const onBlur = () => setFocused(false);
  window.addEventListener("visibilitychange", onVisibility, false);
  window.addEventListener("focus", onFocus, false);
  window.addEventListener("blur", onBlur, false);
  return () => {
    window.removeEventListener("visibilitychange", onVisibility);
    window.removeEventListener("focus", onFocus);
    window.removeEventListener("blur", onBlur);
  };
}

/** Counts the window's focus while the calling component is mounted. */
export function useWindowFocus(): void {
  useEffect(() => {
    focusManager.setEventListener(windowFocus);
    return () => {
      focusManager.setFocused(undefined);
      focusManager.setEventListener(visibility);
    };
  }, []);
}
