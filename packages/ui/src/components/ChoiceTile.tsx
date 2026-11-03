// A large choice between ways to go on (Plan.md P11.11; ADR-0020, U106, R204): a box with a larger
// icon, a short title and one terse line, the whole of it one link named by its title. Hover and focus
// take `--primary`'s border and a light tint of it; the main choice has them from the start.
import { Link, type LinkProps } from "@tanstack/react-router";
import { cn } from "cn";
import type { LucideIcon } from "lucide-react";
import { useId } from "react";

/** A tile leading to `to`; `main` is the stronger accent. */
export function ChoiceTile({
  to,
  icon: Icon,
  title,
  line,
  main = false,
}: {
  to: NonNullable<LinkProps["to"]>;
  icon: LucideIcon;
  title: string;
  line: string;
  main?: boolean;
}) {
  const titleId = useId();
  const lineId = useId();
  return (
    <Link
      to={to}
      aria-labelledby={titleId}
      aria-describedby={lineId}
      data-slot="choice-tile"
      className={cn(
        "flex flex-col gap-2 rounded-lg border bg-card p-4 transition-colors hover:border-primary hover:bg-primary/10 focus-visible:border-primary focus-visible:bg-primary/10",
        main && "border-primary bg-primary/5",
      )}
    >
      <Icon aria-hidden="true" className="size-7 text-primary" />
      <span id={titleId} className="text-base font-semibold">
        {title}
      </span>
      <span id={lineId} className="text-sm text-muted-foreground">
        {line}
      </span>
    </Link>
  );
}
