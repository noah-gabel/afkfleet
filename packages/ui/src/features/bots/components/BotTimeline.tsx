// A bot's timeline (Plan.md P11.9; ADR-0020, U46, S71, R107, R111): what the live record kept
// of it since sign-in, with the gaps of 5 s or more when live updates were down, newest
// first, each at its own time to the second where it has one, else when it came.
import { useId } from "react";

import { type TimelineItem, timeline } from "../../../api/liveRecord";
import { useModes } from "../../../api/queries";
import { useRecord } from "../../../app/context";
import { formatClockSeconds } from "../../../lib/time";
import { modeLabel, modesOf } from "../facts";
import { entryText, gapText, TIMELINE } from "../texts";

/** When `item` happened, as the timeline shows it. */
function timeOf(item: TimelineItem): string {
  return "at" in item && item.at !== undefined ? item.at : new Date(item.arrived).toISOString();
}

/** Bot `id`'s timeline. */
export function BotTimeline({ id }: { id: string }) {
  const record = useRecord((state) => state.record);
  const modes = modesOf(useModes());
  const headingId = useId();
  const items = timeline(record, id);
  const named = (modeId: string) => modeLabel(modeId, modes);
  return (
    <section aria-labelledby={headingId} className="flex flex-col gap-2">
      <h2 id={headingId} className="text-lg font-semibold">
        {TIMELINE.heading}
      </h2>
      <p className="text-xs text-muted-foreground">{TIMELINE.note}</p>
      {items.length === 0 ? (
        <p className="text-sm text-muted-foreground">{TIMELINE.empty}</p>
      ) : (
        <ol className="flex flex-col gap-1 text-sm">
          {items.map((item) => {
            const at = timeOf(item);
            const text = item.kind === "gap" ? gapText(item.downMs) : entryText(item, named);
            // One frame or event makes at most one item of a kind.
            return (
              <li key={`${item.kind}-${item.arrived}-${text}`} className="flex gap-3">
                <time dateTime={at} className="shrink-0 tabular-nums text-muted-foreground">
                  {formatClockSeconds(at)}
                </time>
                <span className="break-words">{text}</span>
              </li>
            );
          })}
        </ol>
      )}
    </section>
  );
}
