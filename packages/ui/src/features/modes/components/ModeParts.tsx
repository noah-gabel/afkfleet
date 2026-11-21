import { Facts } from "../../../components/Facts";
import type { ModeResponse } from "../../../generated/ModeResponse";
import {
  FACT,
  FACT_SCRIPT,
  MARK_ANY_TIME_CHAT,
  MARK_UNLISTED,
  UNKNOWN_STEP,
  whoseText,
} from "../texts";

/** One line for each of `count` steps this version doesn't know (R160). */
export function UnknownSteps({ count }: { count: number }) {
  const lines = Array.from({ length: count }, (_, index) => `step-${index}`);
  return lines.map((line) => (
    <p key={line} className="text-sm">
      {UNKNOWN_STEP}
    </p>
  ));
}

/** What `mode` runs: its steps' count, or a script (P15.25). */
export function bodyFact(mode: ModeResponse): { term: string; value: string } {
  return mode.script === undefined
    ? { term: FACT.steps, value: String(mode.steps.length) }
    : { term: FACT.runs, value: FACT_SCRIPT };
}

/** Whose `mode` is and what it runs, and its marks; its fields say the rest. */
export function ModeSummary({ mode }: { mode: ModeResponse }) {
  return (
    <div className="flex flex-col gap-1">
      <Facts
        facts={[{ term: FACT.owner, value: whoseText(mode.ownership, mode.owner) }, bodyFact(mode)]}
        place="screen"
      />
      {mode.unlisted_command ? <p className="text-sm">{MARK_UNLISTED}</p> : null}
      {mode.script?.any_time_chat ? <p className="text-sm">{MARK_ANY_TIME_CHAT}</p> : null}
    </div>
  );
}
