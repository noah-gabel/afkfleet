// A bot card's Start or Stop button and what it answers (Plan.md P11.11; ADR-0020, U88, S124): only
// where the viewer's level allows and the bot's state offers one. Both sit above the card's stretched
// link, so a click on either stays on Home, and a refusal's request ID can be selected. A View holder
// reads nothing here (U35). There's no password to ask: start and stop need none, on the bot's screen
// too. One button, never keyed by what it does, so the keyboard's place survives Start becoming Stop.
import { cn } from "cn";
import { LoaderCircleIcon } from "lucide-react";

import { ABOVE_LINK } from "../../../components/linkedCard";
import { RetryWaitMessage } from "../../../components/RetryWaitMessage";
import { Button } from "../../../components/ui/button";
import type { BotResponse } from "../../../generated/BotResponse";
import { cardControl } from "../controls";
import type { BotControl } from "../hooks/useBotControl";
import { botActions } from "../permissions";
import { CONTROL, cardControlName } from "../texts";

/** What `bot`'s card offers the viewer, if anything. */
function offered(bot: BotResponse) {
  return botActions(bot.account.level).control ? cardControl(bot) : null;
}

/** The button, at the card's top right. */
export function BotCardButton({ bot, control }: { bot: BotResponse; control: BotControl }) {
  const offer = offered(bot);
  if (offer === null) {
    return null;
  }
  return (
    <Button
      type="button"
      size="sm"
      variant={offer === "start" ? "default" : "outline"}
      aria-label={cardControlName(CONTROL[offer], bot.account.name)}
      disabled={control.locked(offer)}
      className={cn(ABOVE_LINK, "shrink-0")}
      onClick={() => {
        void control.run(offer);
      }}
    >
      {control.running === offer ? (
        <LoaderCircleIcon aria-hidden="true" className="animate-spin" />
      ) : null}
      {CONTROL[offer]}
    </Button>
  );
}

/** What the button's last call answered, by the button. */
export function BotCardAnswer({ bot, control }: { bot: BotResponse; control: BotControl }) {
  return (
    <>
      {control.failure === null ? null : (
        <p role="alert" className={cn(ABOVE_LINK, "text-xs text-destructive")}>
          {control.failure}
        </p>
      )}
      {offered(bot) === null ? null : (
        <div className={cn(ABOVE_LINK, "empty:hidden")}>
          <RetryWaitMessage wait={control.wait} />
        </div>
      )}
    </>
  );
}
