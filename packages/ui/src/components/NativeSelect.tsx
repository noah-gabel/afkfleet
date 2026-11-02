// The app's one native select (Plan.md P11.10; ADR-0020, K's "As built"). WebView2 paints a select's
// open list from its colours and those of its options, so a see-through select in dark mode opened
// a light list with light text; and that list isn't in the DOM, so no test can see it.
// The look is defined here alone, from the theme's tokens, for the select and its options, and it
// takes no `className`, so no screen can undo it. scripts/theme-guard.mjs refuses a `<select>`
// anywhere else, and a look here that loses a token or goes see-through.
import type { ComponentProps } from "react";

const LOOK =
  "h-8 rounded-lg border border-input bg-background px-2.5 text-sm text-foreground disabled:opacity-50 [&_option]:bg-background [&_option]:text-foreground";

/** A `<select>` in the app's one look; everything but `className` passes through. */
export function NativeSelect(props: Omit<ComponentProps<"select">, "className">) {
  return <select {...props} className={LOOK} />;
}
