// The app's toasts (Plan.md P8.6, ADR-0017): sonner, in the app's theme (ADR-0020, U84), never
// through next-themes, whose inline script the CSP would block, with lucide's icons. sonner 2.0.8 is patched (patches/sonner@2.0.8.patch) so it no longer injects its
// stylesheet as a <style> element at import time, which the release CSP (`style-src
// 'self'`) blocks; its CSS comes through styles.css instead. Toaster.test.tsx fails if a
// bump brings the injection back.

import {
  CircleCheckIcon,
  InfoIcon,
  LoaderCircleIcon,
  OctagonXIcon,
  TriangleAlertIcon,
} from "lucide-react";
import type { CSSProperties } from "react";
import { Toaster as Sonner } from "sonner";

import { useTheme } from "../app/context";

// sonner's colour variables, set to the theme's tokens. React applies a `style` prop
// through the CSS object model, which `style-src 'self'` allows.
const THEME = {
  "--normal-bg": "var(--popover)",
  "--normal-text": "var(--popover-foreground)",
  "--normal-border": "var(--border)",
  "--normal-bg-hover": "var(--accent)",
  "--normal-border-hover": "var(--border)",
  "--gray2": "var(--accent)",
  "--gray5": "var(--border)",
  "--border-radius": "var(--radius)",
} as CSSProperties;

// An error or a warning takes a tint and a border of its own (ADR-0020, U91, S116); the rest stay
// plain. sonner's background, close button and action all read these two, so they follow. It isn't
// `richColors`, which would tint every type. The text stays `--foreground`.
//
// Sonner's own stylesheet gives the action a fixed corner and draws a shadow around whatever has the
// focus. These classes win over it, as it's in the components layer: the action's corner comes from
// `--radius` (U101), and the focus shows only styles.css's one outline (S107).
const NO_FOCUS_SHADOW = "focus-visible:[box-shadow:none]";
const TINTS = {
  toast: NO_FOCUS_SHADOW,
  closeButton: NO_FOCUS_SHADOW,
  error:
    "[--normal-bg:color-mix(in_srgb,var(--destructive)_12%,var(--popover))] [--normal-border:var(--destructive)]",
  warning:
    "[--normal-bg:color-mix(in_srgb,var(--warning)_12%,var(--popover))] [--normal-border:var(--warning)]",
  title: "font-semibold",
  actionButton: `rounded-lg font-semibold ${NO_FOCUS_SHADOW}`,
};

/** Where the app's toasts appear; rendered once, next to the router. */
export function Toaster() {
  const theme = useTheme((state) => state.theme);
  return (
    <Sonner
      theme={theme}
      position="bottom-right"
      closeButton
      style={THEME}
      className="font-sans"
      toastOptions={{ classNames: TINTS }}
      icons={{
        success: <CircleCheckIcon className="size-4" />,
        info: <InfoIcon className="size-4" />,
        warning: <TriangleAlertIcon className="size-4" />,
        error: <OctagonXIcon className="size-4" />,
        loading: <LoaderCircleIcon className="size-4 animate-spin" />,
      }}
    />
  );
}
