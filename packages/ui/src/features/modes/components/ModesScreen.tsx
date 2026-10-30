// The modes' list (Plan.md P11.9; ADR-0020, U74, S14, S68): "New mode", then every mode the viewer
// may see, in the API's order, newest first, each a card that opens its own screen: whose it is
// as far as the server says, who sees it, how many steps it holds, and its mark. Every page is
// read at once, the query the bots' screens share.
import { Link } from "@tanstack/react-router";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { useModes } from "../../../api/queries";
import { Button } from "../../../components/ui/button";
import { Card, CardContent, CardHeader } from "../../../components/ui/card";
import type { ModeResponse } from "../../../generated/ModeResponse";
import { modesOf } from "../../bots/facts";
import {
  LOADING_MODES,
  MARK_UNLISTED,
  MODES_HEADING,
  MODES_INTRO,
  modesFailed,
  NEW_MODE,
  openMode,
  stepCount,
  visibilityText,
  whoseText,
} from "../texts";

/** Every mode. */
export function ModesScreen() {
  const query = useModes();
  return (
    <main className="flex max-w-3xl flex-col gap-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <h1 className="text-2xl font-semibold tracking-tight">{MODES_HEADING}</h1>
        <Button asChild>
          <Link to="/modes/new">{NEW_MODE}</Link>
        </Button>
      </div>
      <p className="text-muted-foreground">{MODES_INTRO}</p>
      <Body query={query} />
    </main>
  );
}

/** The list's state: failed, loading, or the cards. */
function Body({ query }: { query: ReturnType<typeof useModes> }) {
  const known = modesOf(query);
  if (known === "failed") {
    return (
      <p role="alert" className="text-sm text-destructive">
        {modesFailed(describeError(toApiError(query.error).error))}
      </p>
    );
  }
  if (known === "loading") {
    return <p className="text-sm text-muted-foreground">{LOADING_MODES}</p>;
  }
  return (
    <ul aria-label={MODES_HEADING} className="flex flex-col gap-3">
      {known.map((mode) => (
        <li key={mode.id}>
          <ModeCard mode={mode} />
        </li>
      ))}
    </ul>
  );
}

/** One mode's card: the whole card is its link, as a bot's on Home is. */
function ModeCard({ mode }: { mode: ModeResponse }) {
  return (
    <Card className="relative gap-1 py-4 transition-colors hover:bg-muted/40">
      <CardHeader>
        <h2 className="text-lg font-semibold">
          <Link
            to="/modes/$id"
            params={{ id: mode.id }}
            aria-label={openMode(mode.name)}
            className="after:absolute after:inset-0 after:content-['']"
          >
            {mode.name}
          </Link>
        </h2>
      </CardHeader>
      <CardContent className="flex flex-col gap-1">
        <p className="text-sm text-muted-foreground">{modeFacts(mode)}</p>
        {mode.unlisted_command ? <p className="text-sm">{MARK_UNLISTED}</p> : null}
      </CardContent>
    </Card>
  );
}

/** Whose `mode` is, who sees it, and how many steps it holds. */
export function modeFacts(mode: ModeResponse): string {
  return [
    whoseText(mode.ownership, mode.owner),
    visibilityText(mode.visibility),
    stepCount(mode.steps.length),
  ].join(" · ");
}
