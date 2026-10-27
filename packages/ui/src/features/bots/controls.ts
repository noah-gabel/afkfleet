// Which lifecycle controls a bot's state offers (Plan.md P11.9; ADR-0019, S50, S51; ADR-0020,
// U38, R94, R103), apart from whether the viewer may use them (permissions.ts):
// - its agent unreachable, or a report awaited: Stop while it should run, else Start;
// - resting Paused or Failed: Resume or Reset, and Stop while it should run;
// - wanted stopped (or a desired state this version doesn't know): Start;
// - running (connecting, signing in, online, backing off): Stop and Restart;
// - anything else that should run (stopping, not placed, a state this version doesn't
//   know): Stop.
import type { BotResponse } from "../../generated/BotResponse";

/** A lifecycle control. */
export type Control = "start" | "stop" | "restart" | "resume" | "reset";

/** The reported states a restart applies to. */
const RUNNING: ReadonlySet<string> = new Set([
  "connecting",
  "awaiting_session",
  "online",
  "backoff",
]);

/** The controls `bot`'s state offers, in the order they show. */
export function controlsFor(bot: Pick<BotResponse, "status" | "desired_state">): Control[] {
  const { status } = bot;
  const running = bot.desired_state === "running";
  if (status.kind !== "reported" && status.kind !== "unplaced") {
    return running ? ["stop"] : ["start"];
  }
  if (status.state === "paused" || status.state === "failed") {
    const again: Control = status.state === "paused" ? "resume" : "reset";
    return running ? [again, "stop"] : [again];
  }
  if (!running) {
    return ["start"];
  }
  return status.kind === "reported" && RUNNING.has(String(status.state))
    ? ["stop", "restart"]
    : ["stop"];
}
