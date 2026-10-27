// A bot's screen (Plan.md P11.9; ADR-0020, U39): its state and its facts, read-only for now,
// in the left of the two columns U39 sets side by side; the chat comes into the right one.
// A read that answers 404 says the bot doesn't exist or can't be seen, also when it goes
// while its screen is open (a `bot_removed` resets its query, which reads it again, R98).
import { Link } from "@tanstack/react-router";
import { useId } from "react";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { useBot, useModes } from "../../../api/queries";
import { Button } from "../../../components/ui/button";
import { useNow } from "../../../hooks/useNow";
import { botFacts, modesOf, uptimeNextChange } from "../facts";
import { botActions } from "../permissions";
import { agentLastSeen, BACK_HOME, BOT_NOT_FOUND } from "../texts";
import { BotControls } from "./BotControls";
import { Facts, StateBadge } from "./BotParts";
import { BotSettings } from "./BotSettings";

/** Bot `id`. */
export function BotScreen({ id }: { id: string }) {
  const bot = useBot(id);
  const modes = useModes();
  const nameId = useId();
  const data = bot.data;
  const now = useNow((at) => (data === undefined ? 60_000 : uptimeNextChange(data, at)));

  // A read that failed shows instead of the bot only before it was ever read, or when it
  // answers 404: the bot went while its screen was open.
  const failure = bot.error === null ? null : toApiError(bot.error).error;
  if (failure !== null && (data === undefined || failure.api?.status === 404)) {
    return (
      <main className="flex max-w-3xl flex-col gap-4 p-6">
        <p role="alert">{failure.api?.status === 404 ? BOT_NOT_FOUND : describeError(failure)}</p>
        <BackLink />
      </main>
    );
  }
  if (data === undefined) {
    return <main className="p-6" />;
  }
  const actions = botActions(data.account.level);
  const facts = botFacts(data, modesOf(modes), now, "screen", {
    mode: actions.chooseMode,
    server: actions.configure,
  });
  const lastSeen = data.status.kind === "unknown" ? data.status.agent_last_seen_at : undefined;
  return (
    <main className="grid gap-6 p-6 lg:grid-cols-2">
      <section aria-labelledby={nameId} className="flex flex-col gap-4">
        <div className="flex flex-wrap items-center gap-3">
          <h1 id={nameId} className="text-2xl font-semibold tracking-tight">
            {data.account.name}
          </h1>
          <StateBadge bot={data} />
        </div>
        <Facts facts={facts} place="screen" />
        {lastSeen === undefined ? null : (
          <p className="text-sm text-muted-foreground">{agentLastSeen(lastSeen, now)}</p>
        )}
        <BotControls bot={data} />
        <BotSettings bot={data} />
        <BackLink />
      </section>
    </main>
  );
}

/** Back to Home. */
function BackLink() {
  return (
    <Button asChild variant="link" className="self-start px-0">
      <Link to="/home">{BACK_HOME}</Link>
    </Button>
  );
}
