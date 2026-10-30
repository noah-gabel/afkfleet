// The modes' screens' texts (Plan.md P11.9; ADR-0020's group I table, U63–U68, U72, U73): the
// list's and the editor's, in Minecraft terms. The server's own messages show as they are.
import { MAX_MODES_PER_OWNER, MODE_MAX_STEPS, MODE_NAME_MAX_CHARS } from "../../api/limits";
import type { ModeActionKind } from "../../generated/ModeActionKind";
import type { ModeOwnership } from "../../generated/ModeOwnership";
import type { ModeVisibility } from "../../generated/ModeVisibility";
import type { UserLabel } from "../../generated/UserLabel";

/** The nav entry's, the list's and the back link's text. */
export const MODES_HEADING = "Modes";
/** What the list says first. */
export const MODES_INTRO = "A mode is what a bot does while it's online.";
/** The list's way to a new mode, and a new mode's heading. */
export const NEW_MODE = "New mode";
/** The list, or a mode, while the modes load. */
export const LOADING_MODES = "Loading modes…";

/** The list, or a mode, when the modes couldn't be read. */
export function modesFailed(why: string): string {
  return `Couldn't load the modes: ${why}`;
}

/** A card's link, for screen readers. */
export function openMode(name: string): string {
  return `Open ${name}`;
}

/** Whose a mode is; `owner` only reaches Admins and the Owner (S14). */
export function whoseText(ownership: ModeOwnership, owner: UserLabel | undefined): string {
  switch (ownership) {
    case "built_in":
      return "Built in";
    case "mine":
      return "Yours";
    default:
      return owner?.username ?? "Someone else's";
  }
}

/** A mode's visibility, as the server says it; a value this version doesn't know shows as sent. */
export function visibilityText(visibility: ModeVisibility): string {
  switch (visibility) {
    case "private":
      return "Private";
    case "shared":
      return "Shared";
    default:
      return visibility;
  }
}

/** How many steps a mode holds. */
export function stepCount(count: number): string {
  if (count === 0) {
    return "No steps";
  }
  return count === 1 ? "1 step" : `${count} steps`;
}

/** A mode that sends a command off the allowlist (S68). */
export const MARK_UNLISTED =
  "Sends a command off the allowlist: only someone who manages a bot can choose this mode.";
/** Beside a command that came with its arguments cut (S15). */
export const MARK_CUT = "Its arguments are hidden: only someone who can edit this mode sees them.";
/** Beside such a command in a copy, while it still reads as it came (U63, R170). */
export const MARK_CUT_IN_COPY = "Add its arguments: the original's are hidden from you.";

export const NAME_FIELD = "Name";
export const VISIBILITY_FIELD = "Who can see it";
/** The switch's choices (U67). */
export const VISIBILITY_PRIVATE = "Private";
export const VISIBILITY_SHARED = "Shared with everyone";
/** On a new mode or a copy, which has no flag to read yet (U67). */
export const STARTS_PRIVATE =
  "New modes start private. If you're allowed to share it, the choice appears after saving.";

/** The sections (U68). */
export const SECTION_TEXT = {
  atStart: {
    heading: "When it joins",
    hint: "Runs once each time the bot joins a server.",
  },
  repeating: {
    heading: "While it's online",
    hint: "Repeats while the bot is online, starting one wait after it joins.",
  },
  onRespawn: {
    heading: "After it respawns",
    hint: "After a death, the bot respawns, lets go of the use button and stops sneaking, then runs these steps. A chat step here is skipped if the last one went out less than 30 seconds ago.",
  },
} as const;

/** The block of the steps whose schedule this version doesn't know (R160). */
export const OTHER_STEPS = "Other steps";
export const ADD_STEP = "Add step";
/** Next to "Add step" once the mode holds the most. */
export const AT_MOST_STEPS = `At most ${MODE_MAX_STEPS} steps.`;

/** A card's label, `n` counted from 1 in its section. */
export function stepLabel(n: number): string {
  return `Step ${n}`;
}

export const REMOVE = "Remove";
export const MOVE_UP = "Move up";
export const MOVE_DOWN = "Move down";

/** What the live region says after a move. */
export function stepMoved(n: number, position: number, count: number): string {
  return `Step ${n} moved to position ${position} of ${count}.`;
}

export const WHAT_IT_DOES = "What it does";
/** Each action's name, for the "What it does" choices, in their order. */
export const ACTION_TEXT: Readonly<Record<ModeActionKind, string>> = {
  look: "Look in a direction",
  rotate_random: "Look around randomly",
  jump: "Jump",
  sneak: "Sneak",
  swing_arm: "Swing arm",
  use_item: "Use the held item (right-click)",
  hold_use: "Hold right-click",
  attack_facing_entity: "Attack what's in front (left-click)",
  select_hotbar_slot: "Select a hotbar slot",
  send_chat: "Send chat or a command",
};

export const YAW_FIELD = "Yaw (left or right, as the F3 screen shows it)";
export const PITCH_FIELD = "Pitch (up or down, as the F3 screen shows it)";
export const MAX_YAW_FIELD = "Turn up to (degrees either way)";
export const MAX_PITCH_FIELD = "Tilt up to (degrees either way)";
export const SNEAK_FIELD = "Sneaking";
export const SNEAK_START = "Start";
export const SNEAK_STOP = "Stop";
export const HOLD_FIELD = "Right-click";
export const HOLD_START = "Hold";
export const HOLD_STOP = "Let go";
export const SLOT_FIELD = "Slot";
export const MESSAGE_FIELD = "Message";
export const EVERY_FIELD = "Every";
export const JITTER_FIELD = "Plus up to, at random";
export const CHANCE_FIELD = "Chance (%)";

/** A time field's unit's drop-down, for screen readers. */
export function unitLabel(field: string): string {
  return `${field}: unit`;
}

/** The time units' names (U72). */
export const UNIT_TEXT = { seconds: "seconds", minutes: "minutes", hours: "hours" } as const;

export const SAVE = "Save";
export const DELETE = "Delete";
export const CANCEL = "Cancel";

/** What the password prompt says when a save holds a command off the allowlist (S16). */
export const SAVE_STEP_UP =
  "Enter your password to save a command off the allowlist. It counts for 5 minutes.";

/** The toast after a create. */
export function created(name: string): string {
  return `Created ${name}.`;
}

/** The toast after a save. */
export const SAVED = "Saved. Bots using this mode switch to the change at once.";
/** The toast after a save that changed nothing (S21, S84). */
export const NOTHING_TO_SAVE = "Nothing to save: the mode already reads like this.";

/** What Delete asks first. */
export function deleteQuestion(name: string): string {
  return `Delete ${name}? This can't be undone.`;
}

export const DELETE_MODE = "Delete mode";

/** The toast after a delete. */
export function deleted(name: string): string {
  return `Deleted ${name}.`;
}

/** A create's 409 `conflict`: the user owns the most modes (S19). */
export const CAP_REACHED = `You already have ${MAX_MODES_PER_OWNER} modes, the most you can have. Delete one to add another.`;

/** After a 409 `mode_changed`, next to the server's own text (U64). */
export const LOAD_AGAIN = "Load again";
/** After a 409 `mode_in_use_by_others`, under the server's own text (U65). */
export const IN_USE_BY_OTHERS =
  "Someone who can control that bot has to switch it to another mode first. Your edits are still here; you can save them as a copy.";

/** A mode that isn't there: never was, or the viewer can't see it. */
export const MODE_GONE = "This mode doesn't exist any more, or you can't see it.";
/** The row left the list while the form was open (S83). */
export const LEFT_THE_LIST =
  "This mode was deleted, or you can no longer see it. Your edits are still here; you can save them as a copy.";
/** `editable` turned false while the form was open (S83). */
export const NO_LONGER_EDITABLE = "You can no longer edit this mode, so your edits weren't kept.";

/** A step of a type this version doesn't know (R160). */
export const UNKNOWN_STEP = "This app doesn't know this step.";
/** A mode holding one. */
export const UNKNOWN_MODE =
  "This mode has a step this app doesn't know, so it opens read-only. Update the app to edit it.";

/** The way to a copy (U63). */
export const MAKE_A_COPY = "Make a copy";
/** A notice's way to keep the edits (U63). */
export const SAVE_AS_COPY = "Save as copy";

/** A copy's suggested name: `name` and " copy", shortened to fit. */
export function copyName(name: string): string {
  const suffix = " copy";
  const kept = [...name].slice(0, MODE_NAME_MAX_CHARS - suffix.length).join("");
  return `${kept.trimEnd()}${suffix}`;
}

/** The toast after a copy. */
export function savedCopy(name: string): string {
  return `Saved a copy as ${name}.`;
}

/** The bots at a `mode_in_use` (U66). */
export const BOTS_THAT_USE_IT = "Bots you can see that use this mode:";
export const A_BOT_YOU_CANT_SEE = "It's a bot you can't see, so its owner has to switch it.";

/** When the bots at a `mode_in_use` couldn't be read. */
export function botsFailed(why: string): string {
  return `Couldn't read your bots: ${why}`;
}

/** The drag handle's name (U68). */
export function dragStep(n: number): string {
  return `Drag step ${n}`;
}

/** What a screen reader hears on the handle (R167). */
export const DRAG_INSTRUCTIONS =
  "To pick up a step, press Space or Enter. Use the arrow keys to move it, then Space or Enter to drop it, or Escape to cancel.";

/** What the live region says when a step is picked up. */
export function pickedUp(n: number): string {
  return `Picked up step ${n}.`;
}

/** What the live region says when a drag is cancelled. */
export function putBack(n: number): string {
  return `Put back step ${n}.`;
}
