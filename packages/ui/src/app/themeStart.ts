// The class on `<html>` before React renders, since `render()` is scheduled and a paint can land
// first (ADR-0020, S92). App.tsx imports this first, so every entry runs it; the CSP allows no
// inline script to do it sooner. attachTheme takes over once the App is mounted.
import { applyTheme, readThemeChoice, resolveTheme, windowsTheme } from "./theme";

// The package's entry reaches Node too, in the e2e suite's specs without a browser.
if (typeof document !== "undefined") {
  applyTheme(resolveTheme(readThemeChoice(), windowsTheme()));
}
