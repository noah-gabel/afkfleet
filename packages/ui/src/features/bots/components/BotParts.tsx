// What a bot's card and its screen share (Plan.md P11.9): its state.
import { StatusLine } from "../../../components/StatusLine";
import type { BotResponse } from "../../../generated/BotResponse";
import { stateLine } from "../texts";

/** `bot`'s state, as its status line. */
export function BotState({ bot }: { bot: Pick<BotResponse, "status" | "desired_state"> }) {
  const line = stateLine(bot);
  return <StatusLine tone={line.tone}>{line.text}</StatusLine>;
}
