// What a mode's editor and its locked view share (Plan.md P11.9; ADR-0020, S14, S68): the way
// back, and the line and mark under the heading.
import { Link } from "@tanstack/react-router";

import { Button } from "../../../components/ui/button";
import type { ModeResponse } from "../../../generated/ModeResponse";
import { MARK_UNLISTED, MODES_HEADING, UNKNOWN_STEP } from "../texts";
import { modeFacts } from "./ModesScreen";

/** One line for each of `count` steps this version doesn't know (R160). */
export function UnknownSteps({ count }: { count: number }) {
  const lines = Array.from({ length: count }, (_, index) => `step-${index}`);
  return lines.map((line) => (
    <p key={line} className="text-sm">
      {UNKNOWN_STEP}
    </p>
  ));
}

/** "Modes": back to the list. */
export function BackLink() {
  return (
    <Button asChild variant="link" className="self-start px-0">
      <Link to="/modes">{MODES_HEADING}</Link>
    </Button>
  );
}

/** Whose `mode` is, who sees it, its steps, and its mark. */
export function ModeSummary({ mode }: { mode: ModeResponse }) {
  return (
    <div className="flex flex-col gap-1">
      <p className="text-sm text-muted-foreground">{modeFacts(mode)}</p>
      {mode.unlisted_command ? <p className="text-sm">{MARK_UNLISTED}</p> : null}
    </div>
  );
}
