// The modes' list (Plan.md P11.9, P11.11; ADR-0020, U74, U119, S14, S68): "New mode" at the top
// right, then every mode the viewer may see, in the API's order, newest first, as a grid of cards,
// as Home's: each card opens its own screen and says whose it is as far as the server says, who
// sees it, how many steps it holds, and its mark. Every page is read at once, the query the bots'
// screens share.
import { Link } from "@tanstack/react-router";
import { PlusIcon } from "lucide-react";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { useModes } from "../../../api/queries";
import { Facts } from "../../../components/Facts";
import { CARD_GRID, CARD_NAME, LINKED_CARD, STRETCHED_LINK } from "../../../components/linkedCard";
import { ScreenState } from "../../../components/ScreenState";
import { Button } from "../../../components/ui/button";
import { Card } from "../../../components/ui/card";
import type { ModeResponse } from "../../../generated/ModeResponse";
import { modesOf } from "../../bots/facts";
import {
  FACT,
  LOADING_MODES,
  MARK_UNLISTED,
  MODES_HEADING,
  MODES_INTRO,
  modesFailed,
  NEW_MODE,
  NO_MODES,
  openMode,
  visibilityText,
  whoseText,
} from "../texts";

/** The way to a new mode, with its icon. */
function NewModeLink() {
  return (
    <Button asChild size="sm">
      <Link to="/modes/new">
        <PlusIcon aria-hidden="true" />
        {NEW_MODE}
      </Link>
    </Button>
  );
}

/** Every mode. */
export function ModesScreen() {
  const query = useModes();
  const known = modesOf(query);
  return (
    <main className="flex flex-1 flex-col gap-4 p-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h1 className="text-lg font-semibold tracking-tight">{MODES_HEADING}</h1>
        {known === "loading" ? null : <NewModeLink />}
      </div>
      <p className="text-sm text-muted-foreground">{MODES_INTRO}</p>
      <Body query={query} known={known} />
    </main>
  );
}

/** The list's state: failed, loading, none, or the cards. */
function Body({
  query,
  known,
}: {
  query: ReturnType<typeof useModes>;
  known: ReturnType<typeof modesOf>;
}) {
  if (known === "failed") {
    return (
      <ScreenState kind="error">
        {modesFailed(describeError(toApiError(query.error).error))}
      </ScreenState>
    );
  }
  if (known === "loading") {
    return <ScreenState kind="loading">{LOADING_MODES}</ScreenState>;
  }
  if (known.length === 0) {
    return (
      <ScreenState kind="empty" action={<NewModeLink />}>
        {NO_MODES}
      </ScreenState>
    );
  }
  return (
    <ul aria-label={MODES_HEADING} className={CARD_GRID}>
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
    <Card className={LINKED_CARD}>
      <h2 className={CARD_NAME}>
        <Link
          to="/modes/$id"
          params={{ id: mode.id }}
          aria-label={openMode(mode.name)}
          className={STRETCHED_LINK}
        >
          {mode.name}
        </Link>
      </h2>
      <Facts facts={modeFacts(mode)} place="card" />
      {mode.unlisted_command ? <p className="text-xs">{MARK_UNLISTED}</p> : null}
    </Card>
  );
}

/** Whose `mode` is, who sees it, and how many steps it holds, each with its label (U119). */
export function modeFacts(mode: ModeResponse) {
  return [
    { term: FACT.owner, value: whoseText(mode.ownership, mode.owner) },
    { term: FACT.visibility, value: visibilityText(mode.visibility) },
    { term: FACT.steps, value: String(mode.steps.length) },
  ];
}
