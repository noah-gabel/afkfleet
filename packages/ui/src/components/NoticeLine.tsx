// One compact line on Home (Plan.md P11.9; ADR-0020, U35): a warning, or a notice, shown
// only while it applies, as one line above the cards, never a large box. Its text, its
// action, and its Dismiss when it has one.
import type { ReactNode } from "react";

import { Button } from "./ui/button";

/** A compact line: `children` the text, `action` a link, `onDismiss` its Dismiss. */
export function NoticeLine({
  children,
  action,
  onDismiss,
}: {
  children: ReactNode;
  action?: ReactNode;
  onDismiss?: () => void;
}) {
  return (
    <div
      role="alert"
      data-slot="notice-line"
      className="flex flex-wrap items-center gap-x-3 gap-y-1 border-l-2 border-primary/60 py-0.5 pl-3 text-sm"
    >
      <span>{children}</span>
      {action}
      {onDismiss === undefined ? null : (
        <Button type="button" size="xs" variant="ghost" onClick={onDismiss}>
          Dismiss
        </Button>
      )}
    </div>
  );
}
