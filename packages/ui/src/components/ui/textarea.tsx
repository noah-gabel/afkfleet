// shadcn/ui textarea (MIT, https://ui.shadcn.com), style radix-nova, copied from
// https://ui.shadcn.com/r/styles/radix-nova/textarea.json on 2026-10-13 (ADR-0017): re-copy
// it the same way, never through the CLI, and repeat these edits.
// Bridge's look (ADR-0020, U91, S107, S114, S115): on `bg-background`, so its border holds 3:1; the focus ring gone, as styles.css draws every outline.
// Edits: `import * as React` → `import type * as React` (Biome's useImportType); Biome's
// formatting and import order.

import { cn } from "cn";
import type * as React from "react";

function Textarea({ className, ...props }: React.ComponentProps<"textarea">) {
  return (
    <textarea
      data-slot="textarea"
      className={cn(
        "flex field-sizing-content min-h-16 w-full rounded-lg border border-input bg-background px-2.5 py-2 text-sm transition-colors outline-none placeholder:text-muted-foreground disabled:cursor-not-allowed disabled:bg-input/50 disabled:opacity-50 aria-invalid:border-destructive aria-invalid:ring-3 aria-invalid:ring-destructive/20 dark:disabled:bg-input/80 dark:aria-invalid:border-destructive/50 dark:aria-invalid:ring-destructive/40",
        className,
      )}
      {...props}
    />
  );
}

export { Textarea };
