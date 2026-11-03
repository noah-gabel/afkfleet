// A bot's screen (Plan.md P11.9, P11.11; ADR-0020, U39, U91, U105, S123): boxes on the left, for its
// name, state, controls and facts, its Settings and its timeline; the console on the right, filling
// the height, the left column scrolling by itself. Under about 800 px of content it stacks, the boxes
// first (U105). That's measured on the content column, not the window, since the sidebar takes a part
// of it (U113). While it's open, its notices are acknowledged (R109).
// A read that answers 404 says the bot doesn't exist or can't be seen, also when it goes
// while its screen is open (a `bot_removed` resets its query, which reads it again, R98).
import { Link } from "@tanstack/react-router";
import { useEffect, useId } from "react";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { useBot, useModes } from "../../../api/queries";
import { useRecord } from "../../../app/context";
import { Facts } from "../../../components/Facts";
import { ScreenState } from "../../../components/ScreenState";
import { Button } from "../../../components/ui/button";
import { Card } from "../../../components/ui/card";
import { useNow } from "../../../hooks/useNow";
import { botFacts, modesOf, uptimeNextChange } from "../facts";
import { botActions } from "../permissions";
import { AGENTS_LINK, agentLastSeen, BOT_NOT_FOUND, LOADING_BOT } from "../texts";
import { BotChat } from "./BotChat";
import { BotControls } from "./BotControls";
import { BotState } from "./BotParts";
import { BotSettings } from "./BotSettings";
import { BotTimeline } from "./BotTimeline";
import { ChatSend } from "./ChatSend";

/** Bot `id`. */
export function BotScreen({ id }: { id: string }) {
  const bot = useBot(id);
  const setOpen = useRecord((state) => state.setOpen);
  // Its notices are acknowledged while it's open (U42, R109).
  useEffect(() => {
    setOpen(id);
    return () => {
      setOpen(null);
    };
  }, [id, setOpen]);
  const modes = useModes();
  const nameId = useId();
  const data = bot.data;
  const now = useNow((at) => (data === undefined ? 60_000 : uptimeNextChange(data, at)));

  // A read that failed shows instead of the bot only before it was ever read, or when it
  // answers 404: the bot went while its screen was open.
  const failure = bot.error === null ? null : toApiError(bot.error).error;
  if (failure !== null && (data === undefined || failure.api?.status === 404)) {
    return (
      <main className="flex max-w-3xl flex-col gap-4 p-4">
        <ScreenState kind="error">
          {failure.api?.status === 404 ? BOT_NOT_FOUND : describeError(failure)}
        </ScreenState>
      </main>
    );
  }
  if (data === undefined) {
    return (
      <main className="flex max-w-3xl flex-col gap-4 p-4">
        <ScreenState kind="loading">{LOADING_BOT}</ScreenState>
      </main>
    );
  }
  const actions = botActions(data.account.level);
  const facts = botFacts(data, modesOf(modes), now, "screen", {
    mode: actions.chooseMode,
    server: actions.configure,
  });
  const lastSeen = data.status.kind === "unknown" ? data.status.agent_last_seen_at : undefined;
  return (
    <main className="flex flex-1 flex-col p-4 @min-[53rem]/content:min-h-0">
      <div className="grid gap-3 @min-[53rem]/content:min-h-0 @min-[53rem]/content:flex-[1_1_0] @min-[53rem]/content:grid-cols-[minmax(18rem,2fr)_3fr]">
        <div className="relative flex min-w-0 flex-col gap-3 @min-[53rem]/content:-m-1 @min-[53rem]/content:overflow-y-auto @min-[53rem]/content:scroll-p-1 @min-[53rem]/content:p-1">
          <section aria-labelledby={nameId}>
            <Card className="gap-3 p-3">
              <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1">
                <h1 id={nameId} className="font-mono text-lg font-semibold break-all">
                  {data.account.name}
                </h1>
                <BotState bot={data} />
              </div>
              <BotControls bot={data} />
              <Facts facts={facts} place="screen" />
              {lastSeen === undefined ? null : (
                <p className="text-sm text-muted-foreground">{agentLastSeen(lastSeen, now)}</p>
              )}
              {data.status.kind === "unknown" && data.agent !== undefined ? (
                <Button asChild variant="link" size="sm" className="self-start px-0">
                  <Link to="/admin/agents">{AGENTS_LINK}</Link>
                </Button>
              ) : null}
            </Card>
          </section>
          <BotSettings bot={data} />
          <BotTimeline id={data.id} />
        </div>
        <BotChat bot={data}>{actions.chat ? <ChatSend bot={data} /> : null}</BotChat>
      </div>
    </main>
  );
}
