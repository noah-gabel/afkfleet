// The parts a bot's card and its screen share (Plan.md P11.9): its state badge and its
// labelled facts.
import { Badge } from "../../../components/ui/badge";
import type { BotResponse } from "../../../generated/BotResponse";
import type { Fact } from "../facts";
import { stateBadge } from "../texts";

/** `bot`'s state, as its badge. */
export function StateBadge({ bot }: { bot: Pick<BotResponse, "status" | "desired_state"> }) {
  const badge = stateBadge(bot);
  return <Badge variant={badge.variant}>{badge.text}</Badge>;
}

/** `facts`, each with its label: small and muted (U35, U37). */
export function Facts({ facts }: { facts: readonly Fact[] }) {
  return (
    <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 text-sm">
      {facts.map((fact) => (
        <div key={fact.term} className="contents">
          <dt className="text-muted-foreground">{fact.term}</dt>
          <dd className="break-words">{fact.value}</dd>
        </div>
      ))}
    </dl>
  );
}
