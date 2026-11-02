// What a whole section shows instead of its content (Plan.md P11.11; ADR-0020, S117): while it
// loads, when there's nothing yet, or when it couldn't be read. A form's error stays with its form.
import type { ReactNode } from "react";

/** Why the content isn't there. */
export type ScreenStateKind = "loading" | "empty" | "error";

const ROLE: Record<ScreenStateKind, "status" | "alert" | undefined> = {
  loading: "status",
  error: "alert",
  empty: undefined,
};

/** `children` says what's going on; `action` is what can be done about it. */
export function ScreenState({
  kind,
  children,
  action,
}: {
  kind: ScreenStateKind;
  children: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div
      data-slot="screen-state"
      className="flex flex-col items-start gap-2 rounded-lg border bg-card p-3 text-sm"
    >
      <p
        role={ROLE[kind]}
        className={kind === "error" ? "text-destructive" : "text-muted-foreground"}
      >
        {children}
      </p>
      {action}
    </div>
  );
}
