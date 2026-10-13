// shadcn/ui label (MIT, https://ui.shadcn.com), style radix-nova, copied from
// https://ui.shadcn.com/r/styles/radix-nova/label.json on 2026-10-13 (ADR-0017): re-copy
// it the same way, never through the CLI, and repeat these edits.
// Edits: `"use client"` dropped (rsc is false), as the CLI does; `radix-ui`'s `Label` →
// `@radix-ui/react-label`; `import * as React` → `import type * as React` (Biome's
// useImportType); Biome's formatting and import order.

import * as LabelPrimitive from "@radix-ui/react-label";
import { cn } from "cn";
import type * as React from "react";

function Label({ className, ...props }: React.ComponentProps<typeof LabelPrimitive.Root>) {
  return (
    <LabelPrimitive.Root
      data-slot="label"
      className={cn(
        "flex items-center gap-2 text-sm leading-none font-medium select-none group-data-[disabled=true]:pointer-events-none group-data-[disabled=true]:opacity-50 peer-disabled:cursor-not-allowed peer-disabled:opacity-50",
        className,
      )}
      {...props}
    />
  );
}

export { Label };
