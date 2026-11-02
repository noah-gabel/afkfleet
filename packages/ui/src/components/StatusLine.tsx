// A state at a glance (Plan.md P11.11; ADR-0020, U91, S113): a small square and an uppercase mono
// word, from the status tokens. The word always shows, so colour never says it alone, and the
// capitals are the stylesheet's, so a screen reader reads a word, not letters. Whoever knows the
// state gives its tone, never from the word, and an unknown state is `neutral`.
import { cn } from "cn";
import type { ReactNode } from "react";

/** What a state means at a glance. */
export type Tone = "success" | "info" | "warning" | "destructive" | "neutral";

const WORD: Record<Tone, string> = {
  success: "text-success",
  info: "text-info",
  warning: "text-warning",
  destructive: "text-destructive",
  neutral: "text-muted-foreground",
};

const SQUARE: Record<Tone, string> = {
  success: "bg-success",
  info: "bg-info",
  warning: "bg-warning",
  destructive: "bg-destructive",
  neutral: "border border-muted-foreground",
};

/** `children`, the state's word, in `tone`. */
export function StatusLine({ tone, children }: { tone: Tone; children: ReactNode }) {
  return (
    <span
      data-slot="status-line"
      className={cn(
        "inline-flex items-center gap-1.5 font-mono text-[0.6875rem] font-semibold tracking-wider uppercase",
        WORD[tone],
      )}
    >
      <span aria-hidden className={cn("size-2 shrink-0 rounded-sm", SQUARE[tone])} />
      {children}
    </span>
  );
}
