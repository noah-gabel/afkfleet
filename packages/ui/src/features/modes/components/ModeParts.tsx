import { Facts } from "../../../components/Facts";
import type { ModeResponse } from "../../../generated/ModeResponse";
import { FACT, MARK_UNLISTED, UNKNOWN_STEP, whoseText } from "../texts";

/** One line for each of `count` steps this version doesn't know (R160). */
export function UnknownSteps({ count }: { count: number }) {
  const lines = Array.from({ length: count }, (_, index) => `step-${index}`);
  return lines.map((line) => (
    <p key={line} className="text-sm">
      {UNKNOWN_STEP}
    </p>
  ));
}

/** Whose `mode` is and how many steps it holds, and its mark; its fields say the rest. */
export function ModeSummary({ mode }: { mode: ModeResponse }) {
  return (
    <div className="flex flex-col gap-1">
      <Facts
        facts={[
          { term: FACT.owner, value: whoseText(mode.ownership, mode.owner) },
          { term: FACT.steps, value: String(mode.steps.length) },
        ]}
        place="screen"
      />
      {mode.unlisted_command ? <p className="text-sm">{MARK_UNLISTED}</p> : null}
    </div>
  );
}
