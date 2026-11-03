// Labelled facts, on a card and on a screen (Plan.md P11.9, P11.11; ADR-0020, U35, U37, R210): a list
// of a small muted label and its value. On a card the values are small and muted too, under the
// larger name. A fact may lead somewhere, by a link after its value that sits above a card's own
// link (U116).
import { Link } from "@tanstack/react-router";

import { ABOVE_LINK } from "./linkedCard";

/** Where a fact leads: the agents, for a bot that waits on one (U116). */
export interface FactLink {
  readonly to: "/admin/agents";
  readonly label: string;
}

/** One fact: its label, its value, and where it leads, if anywhere. */
export interface Fact {
  readonly term: string;
  readonly value: string;
  readonly link?: FactLink;
}

/** Where the facts show: a card only what matters at a glance (U35, U37). */
export type Place = "card" | "screen";

/** `facts`, each with its label. */
export function Facts({ facts, place }: { facts: readonly Fact[]; place: Place }) {
  const size = place === "card" ? "text-xs" : "text-sm";
  return (
    <dl className={`grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 ${size}`}>
      {facts.map((fact) => (
        <div key={fact.term} className="contents">
          <dt className="text-muted-foreground">{fact.term}</dt>
          <dd className={place === "card" ? "break-words text-muted-foreground" : "break-words"}>
            {fact.value}
            {fact.link === undefined ? null : (
              <>
                {" "}
                <Link
                  to={fact.link.to}
                  className={`${ABOVE_LINK} text-primary underline-offset-4 hover:underline`}
                >
                  {fact.link.label}
                </Link>
              </>
            )}
          </dd>
        </div>
      ))}
    </dl>
  );
}
