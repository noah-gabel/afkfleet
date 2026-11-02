// shadcn/ui table (MIT, https://ui.shadcn.com), style radix-nova, copied from
// https://ui.shadcn.com/r/styles/radix-nova/table.json on 2026-11-02 (ADR-0017): re-copy
// it the same way, never through the CLI, and repeat these edits.
// Edits: `"use client"` dropped (rsc is false), as the CLI does; `import * as React` → `import type
// * as React` (Biome's useImportType); Biome's formatting and import order; Bridge's look (U91): the
// container is the box, headings small and uppercase, hairlines between rows, no row hover, text
// that wraps rather than stays on one line, `scope="col"` on a head; the footer and the caption
// left out, as nothing uses them; `drop` on a head and a cell, and `TableSpanRow` (ADR-0020, S112).

import { cn } from "cn";
import type * as React from "react";

/**
 * Below which width of the table's own box a column drops. A container query, so the sidebar's
 * jump at 1000 px counts: 810 px of box is U91's 1050 px window with the wide sidebar and its
 * padding. Give a head and its cells the same value. Never drop the first column, a row's controls
 * or anything a row is found by: a row's accessible name comes from the cells that stay.
 */
export type TableDrop = "under-810";

const DROP: Record<TableDrop, string> = {
  "under-810": "@max-[50.625rem]/table:hidden",
};

function Table({ className, ...props }: React.ComponentProps<"table">) {
  return (
    <div
      data-slot="table-container"
      className="@container/table relative w-full overflow-x-auto rounded-lg border bg-card"
    >
      <table
        data-slot="table"
        className={cn("w-full text-left text-[0.8125rem]", className)}
        {...props}
      />
    </div>
  );
}

function TableHeader({ className, ...props }: React.ComponentProps<"thead">) {
  return (
    <thead
      data-slot="table-header"
      className={cn("text-[0.6875rem] tracking-wider text-muted-foreground uppercase", className)}
      {...props}
    />
  );
}

function TableBody({ className, ...props }: React.ComponentProps<"tbody">) {
  return <tbody data-slot="table-body" className={className} {...props} />;
}

function TableRow({ className, ...props }: React.ComponentProps<"tr">) {
  return (
    <tr
      data-slot="table-row"
      className={cn("border-t align-middle first:border-t-0 [thead_&]:border-t-0", className)}
      {...props}
    />
  );
}

function TableHead({
  className,
  drop,
  ...props
}: React.ComponentProps<"th"> & { drop?: TableDrop }) {
  return (
    <th
      data-slot="table-head"
      scope="col"
      className={cn(
        "px-3 py-1.5 text-left align-middle font-semibold",
        drop === undefined ? undefined : DROP[drop],
        className,
      )}
      {...props}
    />
  );
}

function TableCell({
  className,
  drop,
  ...props
}: React.ComponentProps<"td"> & { drop?: TableDrop }) {
  return (
    <td
      data-slot="table-cell"
      className={cn(
        "px-3 py-1.5 align-middle",
        drop === undefined ? undefined : DROP[drop],
        className,
      )}
      {...props}
    />
  );
}

/**
 * A row of one cell over `span` columns, for a question or a prompt about the row above. `w-0
 * min-w-full` keeps its text from widening columns a window's width has left out.
 */
function TableSpanRow({
  span,
  children,
  className,
}: {
  span: number;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <TableRow>
      <TableCell colSpan={span} className={className}>
        <div className="w-0 min-w-full">{children}</div>
      </TableCell>
    </TableRow>
  );
}

export { Table, TableBody, TableCell, TableHead, TableHeader, TableRow, TableSpanRow };
