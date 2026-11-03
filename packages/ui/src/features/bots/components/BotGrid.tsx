// The bots on Home (Plan.md P11.9, P11.11; ADR-0020, U35, U37, U88, U91): a grid of cards, newest
// first, a page at a time with "Show more"; one clear next step when there are none. Each card shows
// the bot's name in mono, its Start or Stop button at the top right where the viewer may use it, its
// status line, then its facts, labelled, small and muted. The rest of the card opens the bot's screen.
// The cards follow the live events through their queries (S60), and the uptimes count on (R93).
import { Link } from "@tanstack/react-router";
import type { ReactNode } from "react";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { useBots, useModes } from "../../../api/queries";
import { Facts } from "../../../components/Facts";
import { CARD_GRID, CARD_NAME, LINKED_CARD, STRETCHED_LINK } from "../../../components/linkedCard";
import { ScreenState } from "../../../components/ScreenState";
import { Button } from "../../../components/ui/button";
import { Card } from "../../../components/ui/card";
import type { BotResponse } from "../../../generated/BotResponse";
import { useNow } from "../../../hooks/useNow";
import { botFacts, type Modes, modesOf, uptimeNextChange } from "../facts";
import { useBotControl } from "../hooks/useBotControl";
import {
  BOTS_HEADING,
  botsFailed,
  LINK_AN_ACCOUNT,
  LOADING_BOTS,
  NO_BOTS,
  openBot,
} from "../texts";
import { BotCardAnswer, BotCardButton } from "./BotCardControl";
import { BotState } from "./BotParts";

/** Home's bots. */
export function BotGrid() {
  const bots = useBots();
  const modes = useModes();
  const items = bots.data?.pages.flatMap((page) => page.items) ?? [];
  // The quickest tick any shown uptime needs.
  const now = useNow((at) => Math.min(...items.map((bot) => uptimeNextChange(bot, at))));
  const known = modesOf(modes);
  return (
    <section className="flex flex-col gap-3">
      <h1 className="text-lg font-semibold tracking-tight">{BOTS_HEADING}</h1>
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
      <ScreenState kind="error">
        {botsFailed(describeError(toApiError(query.error).error))}
      </ScreenState>
    );
  }
  if (query.data === undefined) {
    return <ScreenState kind="loading">{LOADING_BOTS}</ScreenState>;
  }
  if (items.length === 0) {
    return (
      <ScreenState
        kind="empty"
        action={
          <Button asChild>
            <Link to="/accounts/link">{LINK_AN_ACCOUNT}</Link>
          </Button>
        }
      >
        {NO_BOTS}
      </ScreenState>
    );
  }
  return (
    <>
      <ul className={CARD_GRID}>
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
        // Asking for a page while the list is read again would cancel that read (TanStack's default).
        <Button
          type="button"
          variant="outline"
          className="self-start"
          disabled={query.isFetching}
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

/** One bot's card: the whole of it is its link, but for its button (U35, U88). */
function BotCard({ bot, modes, now }: { bot: BotResponse; modes: Modes; now: number }) {
  const name = bot.account.name;
  const control = useBotControl(bot, { onCard: true });
  return (
    <Card className={LINKED_CARD}>
      <div className="flex min-h-7 items-center justify-between gap-2">
        <h2 className={CARD_NAME}>
          <Link
            to="/bots/$id"
            params={{ id: bot.id }}
            aria-label={openBot(name)}
            className={STRETCHED_LINK}
          >
            {name}
          </Link>
        </h2>
        <BotCardButton bot={bot} control={control} />
      </div>
      <BotCardAnswer bot={bot} control={control} />
      <BotState bot={bot} />
      <Facts facts={botFacts(bot, modes, now, "card")} place="card" />
    </Card>
  );
}
