// The parts a bot's card and its screen share (Plan.md P11.9): its state badge and its
// labelled facts.
import { Badge } from "../../../components/ui/badge";
import type { BotResponse } from "../../../generated/BotResponse";
import type { Fact, Place } from "../facts";
import { stateBadge } from "../texts";

/** `bot`'s state, as its badge. */
export function StateBadge({ bot }: { bot: Pick<BotResponse, "status" | "desired_state"> }) {
  const badge = stateBadge(bot);
  return <Badge variant={badge.variant}>{badge.text}</Badge>;
}

/**
 * `facts`, each with its small muted label; on a card the values are small and muted too,
 * under the bot's larger name (U35, U37).
 */
export function Facts({ facts, place }: { facts: readonly Fact[]; place: Place }) {
  const size = place === "card" ? "text-xs" : "text-sm";
  return (
    <dl className={`grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 ${size}`}>
      {facts.map((fact) => (
        <div key={fact.term} className="contents">
          <dt className="text-muted-foreground">{fact.term}</dt>
          <dd className={place === "card" ? "break-words text-muted-foreground" : "break-words"}>
            {fact.value}
          </dd>
        </div>
      ))}
    </dl>
  );
}
