// The bots on Home (Plan.md P11.9; ADR-0020, U35, U37): a roomy grid of cards, newest
// first, a page at a time with "Show more"; one clear next step when there are none. Each
// card shows the bot's name and its state badge, then its facts, labelled, small and muted,
// with no buttons: the whole card opens the bot's screen. The cards follow the live events
// through their queries (S60), and the uptimes count on (R93).
import { Link } from "@tanstack/react-router";
import type { ReactNode } from "react";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { useBots, useModes } from "../../../api/queries";
import { Button } from "../../../components/ui/button";
import { Card, CardContent, CardHeader } from "../../../components/ui/card";
import type { BotResponse } from "../../../generated/BotResponse";
import type { ModeResponse } from "../../../generated/ModeResponse";
import { useNow } from "../../../hooks/useNow";
import { botFacts, uptimeTick } from "../facts";
import {
  BOTS_HEADING,
  botsFailed,
  LINK_AN_ACCOUNT,
  LOADING_BOTS,
  NO_BOTS,
  openBot,
} from "../texts";
import { Facts, StateBadge } from "./BotParts";

/** Home's bots. */
export function BotGrid() {
  const bots = useBots();
  const modes = useModes();
  const items = bots.data?.pages.flatMap((page) => page.items) ?? [];
  // The quickest tick any shown uptime needs.
  const now = useNow(Math.min(60_000, ...items.map((bot) => uptimeTick(bot, Date.now()))));
  const known = modes.data?.pages.flatMap((page) => page.items);
  return (
    <section className="flex flex-col gap-4">
      <h1 className="text-2xl font-semibold tracking-tight">{BOTS_HEADING}</h1>
      <Body
        query={bots}
        items={items}
        render={(bot) => <BotCard bot={bot} modes={known} now={now} />}
      />
    </section>
  );
}

/** The list's state: failed, loading, empty, or the cards with "Show more". */
function Body({
  query,
  items,
  render,
}: {
  query: ReturnType<typeof useBots>;
  items: readonly BotResponse[];
  render: (bot: BotResponse) => ReactNode;
}) {
  if (query.error !== null && query.data === undefined) {
    return (
      <p role="alert" className="text-sm text-destructive">
        {botsFailed(describeError(toApiError(query.error).error))}
      </p>
    );
  }
  if (query.data === undefined) {
    return <p className="text-sm text-muted-foreground">{LOADING_BOTS}</p>;
  }
  if (items.length === 0) {
    return (
      <div className="flex flex-col items-start gap-3">
        <p>{NO_BOTS}</p>
        <Button asChild>
          <Link to="/accounts/link">{LINK_AN_ACCOUNT}</Link>
        </Button>
      </div>
    );
  }
  return (
    <>
      <ul className="grid gap-4 [grid-template-columns:repeat(auto-fill,minmax(18rem,1fr))]">
        {items.map((bot) => (
          <li key={bot.id}>{render(bot)}</li>
        ))}
      </ul>
      {query.isFetchNextPageError ? (
        <p role="alert" className="text-sm text-destructive">
          {describeError(toApiError(query.error).error)}
        </p>
      ) : null}
      {query.hasNextPage ? (
        <Button
          type="button"
          variant="outline"
          className="self-start"
          disabled={query.isFetchingNextPage}
          onClick={() => {
            void query.fetchNextPage();
          }}
        >
          Show more
        </Button>
      ) : null}
    </>
  );
}

/** One bot's card: the whole card is its link (U35). */
function BotCard({
  bot,
  modes,
  now,
}: {
  bot: BotResponse;
  modes: readonly ModeResponse[] | undefined;
  now: number;
}) {
  const name = bot.account.name;
  return (
    <Card className="relative h-full gap-3 py-4 transition-colors hover:bg-muted/40">
      <CardHeader className="flex flex-row flex-wrap items-center justify-between gap-2">
        <h2 className="font-semibold">
          <Link
            to="/bots/$id"
            params={{ id: bot.id }}
            aria-label={openBot(name)}
            className="after:absolute after:inset-0 after:content-['']"
          >
            {name}
          </Link>
        </h2>
        <StateBadge bot={bot} />
      </CardHeader>
      <CardContent>
        <Facts facts={botFacts(bot, modes, now, "card")} />
      </CardContent>
    </Card>
  );
}
