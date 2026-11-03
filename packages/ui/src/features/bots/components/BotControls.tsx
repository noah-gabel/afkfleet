// A bot's lifecycle controls on its screen (Plan.md P11.9; ADR-0020, U38, S67, R103): the controls its
// state offers (controls.ts) that the viewer's level allows (permissions.ts); a viewer whose level
// allows none reads one muted line instead. What a call does is useBotControl's.
import { LoaderCircleIcon } from "lucide-react";

import { RetryWaitMessage } from "../../../components/RetryWaitMessage";
import { Button } from "../../../components/ui/button";
import type { BotResponse } from "../../../generated/BotResponse";
import { controlsFor } from "../controls";
import { useBotControl } from "../hooks/useBotControl";
import { botActions } from "../permissions";
import { CONTROL, SAVED_NOTE, SENT_NOTE, VIEW_ONLY } from "../texts";

/** `bot`'s controls. */
export function BotControls({ bot }: { bot: BotResponse }) {
  const control = useBotControl(bot);

  if (!botActions(bot.account.level).control) {
    return <p className="text-sm text-muted-foreground">{VIEW_ONLY}</p>;
  }

  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap gap-2">
        {controlsFor(bot).map((offered) => (
          <Button
            key={offered}
            type="button"
            variant={offered === "start" ? "default" : "outline"}
            size="sm"
            disabled={control.locked(offered)}
            onClick={() => {
              void control.run(offered);
            }}
          >
            {control.running === offered ? (
              <LoaderCircleIcon aria-hidden="true" className="animate-spin" />
            ) : null}
            {CONTROL[offered]}
          </Button>
        ))}
      </div>
      {control.sent ? (
        <p role="status" className="text-sm text-muted-foreground">
          {SENT_NOTE}
        </p>
      ) : null}
      {control.saved ? (
        <p role="status" className="text-sm text-muted-foreground">
          {SAVED_NOTE}
        </p>
      ) : null}
      {control.failure === null ? null : (
        <p role="alert" className="text-sm text-destructive">
          {control.failure}
        </p>
      )}
      <RetryWaitMessage wait={control.wait} />
    </div>
  );
}
