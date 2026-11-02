// A name for a control that shows only an icon (Plan.md P11.11; ADR-0020, S105), since `title` never
// shows on keyboard focus. It opens on hover and when Tab reaches the control (the Tab's keyup lands
// there), not on focus as such: a click, a script or the window coming back says nothing about what
// the user wants named. Escape closes it, and the pointer can rest on the tip itself. It's `fixed`,
// so a scrolling list can't clip it, placed through React's `style`, which the CSP allows. It's only
// seen: the control's own name is what's read, once. `className` goes on the box around the control,
// `tipClassName` on the tip, so a form of the frame can leave the tip out.
//
// The box listens natively: it only watches what happens to the control inside it, and has no role
// of its own to give JSX handlers.
import { cn } from "cn";
import { type ReactElement, useEffect, useRef, useState } from "react";

/** `children`, one control, with `label` as its tip. */
export function Tooltip({
  label,
  children,
  className,
  tipClassName,
}: {
  label: string;
  children: ReactElement;
  className?: string;
  tipClassName?: string;
}) {
  const box = useRef<HTMLSpanElement>(null);
  const [at, setAt] = useState<{ left: number; top: number } | null>(null);
  useEffect(() => {
    const held = box.current;
    if (held === null) {
      return;
    }
    const open = () => {
      const rect = held.getBoundingClientRect();
      setAt({ left: rect.right, top: rect.top + rect.height / 2 });
    };
    const close = () => setAt(null);
    const onKeyUp = (event: KeyboardEvent) => {
      if (event.key === "Tab") {
        open();
      }
    };
    held.addEventListener("pointerenter", open);
    held.addEventListener("pointerleave", close);
    held.addEventListener("pointerdown", close);
    held.addEventListener("keyup", onKeyUp);
    held.addEventListener("focusout", close);
    return () => {
      held.removeEventListener("pointerenter", open);
      held.removeEventListener("pointerleave", close);
      held.removeEventListener("pointerdown", close);
      held.removeEventListener("keyup", onKeyUp);
      held.removeEventListener("focusout", close);
    };
  }, []);
  const isOpen = at !== null;
  useEffect(() => {
    if (!isOpen) {
      return;
    }
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setAt(null);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [isOpen]);
  return (
    <span ref={box} className={cn("flex", className)}>
      {children}
      {at === null ? null : (
        // The transparent padding bridges the gap, so the pointer can reach the tip.
        <span
          data-slot="tooltip"
          aria-hidden="true"
          className={cn("fixed z-50 -translate-y-1/2 pl-2", tipClassName)}
          style={{ left: at.left, top: at.top }}
        >
          <span className="block rounded-md border bg-popover px-2 py-1 text-xs whitespace-nowrap text-popover-foreground">
            {label}
          </span>
        </span>
      )}
    </span>
  );
}
