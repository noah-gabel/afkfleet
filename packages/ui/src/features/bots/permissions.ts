// The bot screens' mirror of `authorize()` (Plan.md P11.9; ADR-0020, S67): only what the
// server would allow is offered. Each right is a threshold on the viewer's own `level` on the
// bot's account, which the server reports (their grant or their implicit Manage, whichever
// is higher); it never copies the rules the server alone can compute (S14). Whether a mode
// needs Manage is the mode's own `unlisted_command` (S68). The address and the agent show
// where the server sent them, never by a rule of the UI's (S69). The test reads fleet-core's
// authorization matrix itself, so a rule change on the server fails it until this file follows.
import type { GrantLevel } from "../../generated/GrantLevel";
import type { ModeResponse } from "../../generated/ModeResponse";

/** What a viewer may do with a bot. */
export interface BotActions {
  /** Start, stop, restart, resume and reset it. */
  readonly control: boolean;
  /** Choose a mode that sends no command off the allowlist. */
  readonly chooseMode: boolean;
  /** Choose any mode, one that sends a command off the allowlist included. */
  readonly chooseAnyMode: boolean;
  /** Set its server address. */
  readonly configure: boolean;
  /** Send plain chat, or a command on the allowlist. */
  readonly chat: boolean;
  /** Send a command off the allowlist. */
  readonly command: boolean;
}

/** What the viewer whose own level on the bot's account is `level` may do with the bot. */
export function botActions(level: GrantLevel): BotActions {
  // Control and Manage control the bot, choose a listed mode and chat; only Manage does the
  // rest. An unknown level is neither.
  const controls = level === "control" || level === "manage";
  const manages = level === "manage";
  return {
    control: controls,
    chooseMode: controls,
    chooseAnyMode: manages,
    configure: manages,
    chat: controls,
    command: manages,
  };
}

/** Whether a viewer who may do `actions` may choose `mode` for the bot. */
export function mayChoose(
  mode: Pick<ModeResponse, "unlisted_command">,
  actions: BotActions,
): boolean {
  return mode.unlisted_command ? actions.chooseAnyMode : actions.chooseMode;
}
