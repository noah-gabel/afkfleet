import type { ModeResponse } from "../../../generated/ModeResponse";
import { MARK_UNLISTED, UNKNOWN_STEP } from "../texts";
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

/** Whose `mode` is, who sees it, its steps, and its mark. */
export function ModeSummary({ mode }: { mode: ModeResponse }) {
  return (
    <div className="flex flex-col gap-1">
      <p className="text-sm text-muted-foreground">{modeFacts(mode)}</p>
      {mode.unlisted_command ? <p className="text-sm">{MARK_UNLISTED}</p> : null}
    </div>
  );
}
