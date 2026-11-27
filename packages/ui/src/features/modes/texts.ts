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
export const MODES_INTRO = "What a bot does while online.";
/** The list's way to a new mode, and a new mode's heading. */
export const NEW_MODE = "New mode";
/** The list while the modes load. */
export const LOADING_MODES = "Loading modes…";
/** A mode's screen while the modes load. */
export const LOADING_MODE = "Loading mode…";
/** The list with no mode in it. */
export const NO_MODES = "No modes.";

/** The list, or a mode, when the modes couldn't be read. */
export function modesFailed(why: string): string {
  return `Couldn't load modes: ${why}`;
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

/** A card's labelled facts (U119). */
export const FACT = {
  owner: "Owner",
  visibility: "Visibility",
  steps: "Steps",
  runs: "Runs",
} as const;

/** A mode that sends a command off the allowlist (S68). */
export const MARK_UNLISTED = "Sends a command off the allowlist. Needs Manage to choose.";
/** Beside a command that came with its arguments cut (S15). */
export const MARK_CUT = "Arguments hidden. Only its editors see them.";
/** Beside such a command in a copy, while it still reads as it came (U63, R170). */
export const MARK_CUT_IN_COPY = "Add its arguments. The original's are hidden from you.";

export const NAME_FIELD = "Name";
export const VISIBILITY_FIELD = "Who can see it";
/** The switch's choices (U67). */
export const VISIBILITY_PRIVATE = "Private";
export const VISIBILITY_SHARED = "Shared with everyone";
/** On a new mode or a copy, which has no flag to read yet (U67). */
export const STARTS_PRIVATE =
  "Starts private. If you may share it, the choice appears after saving.";

/** The sections (U68). */
export const SECTION_TEXT = {
  atStart: {
    heading: "When it joins",
    hint: "Runs once on each join.",
  },
  repeating: {
    heading: "While it's online",
    hint: "Repeats while online, starting one wait after joining.",
  },
  onRespawn: {
    heading: "After it respawns",
    hint: "After a death: respawns, releases the use button, stops sneaking, then runs these. A chat step is skipped within 30 s of the last.",
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
  return `Step ${n} moved to ${position} of ${count}.`;
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

export const YAW_FIELD = "Yaw (left or right, as F3 shows it)";
export const PITCH_FIELD = "Pitch (up or down, as F3 shows it)";
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

/** The toast after a create. */
export function created(name: string): string {
  return `Created ${name}.`;
}

/** The toast after a save. */
export const SAVED = "Saved. Bots using it switch at once.";
/** The toast after a save that changed nothing (S21, S84). */
export const NOTHING_TO_SAVE = "Nothing to save.";

/** What Delete asks first. */
export function deleteQuestion(name: string): string {
  return `Delete ${name}? Can't be undone.`;
}

export const DELETE_MODE = "Delete mode";

/** The toast after a delete. */
export function deleted(name: string): string {
  return `Deleted ${name}.`;
}

/** A create's 409 `conflict`: the user owns the most modes (S19). */
export const CAP_REACHED = `Mode limit reached (${MAX_MODES_PER_OWNER}). Delete one to add another.`;

/** After a 409 `mode_changed`, next to the server's own text (U64). */
export const LOAD_AGAIN = "Load again";
/** After a 409 `mode_in_use_by_others`, under the server's own text (U65). */
export const IN_USE_BY_OTHERS =
  "Someone who controls that bot must switch it first. Your edits are kept: save them as a copy.";

/** A mode that isn't there: never was, or the viewer can't see it. */
export const MODE_GONE = "Mode not found or not available to you.";
/** The row left the list while the form was open (S83). */
export const LEFT_THE_LIST =
  "Mode deleted or no longer visible to you. Your edits are kept: save them as a copy.";
/** `editable` turned false while the form was open (S83). */
export const NO_LONGER_EDITABLE = "You can no longer edit this mode. Your edits weren't kept.";

/** A step of a type this version doesn't know (R160). */
export const UNKNOWN_STEP = "Step unknown to this app.";
/** A mode holding one. */
export const UNKNOWN_MODE =
  "Has a step this app doesn't know: read-only. Update the app to edit it.";

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
  return `Copy saved as ${name}.`;
}

/** The bots at a `mode_in_use` (U66). */
export const BOTS_THAT_USE_IT = "Bots you can see using it:";
export const A_BOT_YOU_CANT_SEE = "A bot you can't see. Its owner must switch it.";

/** When the bots at a `mode_in_use` couldn't be read. */
export function botsFailed(why: string): string {
  return `Couldn't load bots: ${why}`;
}

/** The drag handle's name (U68). */
export function dragStep(n: number): string {
  return `Drag step ${n}`;
}

/** What a screen reader hears on the handle (R167). */
export const DRAG_INSTRUCTIONS =
  "Space or Enter picks up a step. Arrow keys move it; Space or Enter drops it, Escape cancels.";

/** What the live region says when a step is picked up. */
export function pickedUp(n: number): string {
  return `Picked up step ${n}.`;
}

/** What the live region says when a drag is cancelled. */
export function putBack(n: number): string {
  return `Put back step ${n}.`;
}

// --- A scripted mode (Plan.md P15.25, P15.35; ADR-0036, D8, D10, D11, D15; ADR-0037, C3, C5;
// ADR-0042, G13; ADR-0045) ---------------------------------------------------------------------

/** What a mode runs: its kind, chosen when it's made. */
export const KIND_FIELD = "What it runs";
export const KIND_TEXT = { steps: "Steps", script: "A script" } as const;
/** A mode's kind, as its facts show it. */
export const FACT_SCRIPT = "Script";

/** The script's section. */
export const SCRIPT_HEADING = "Script";
export const SCRIPT_HINT =
  "Functions only: the hooks the bot calls, such as on_join(first), and your own. Nothing outside a function runs.";
/** The starter templates: the reference's examples. */
export const TEMPLATE_FIELD = "Start from an example";
export const TEMPLATE_NONE = "Choose an example";
export const TEMPLATE_USE = "Use example";
export const TEMPLATE_HINT =
  "Replaces the script, its safety settings, its messages and its chat patterns.";

/** The safety settings. */
export const SAFETY_HEADING = "Safety";
export const SAFETY_HINT = "Below the script, whatever it does.";
export const LEASH_FIELD = "Leash, in blocks";
export const LEASH_HINT = "How far the bot may walk from where it was put: 1 to 64.";
export const TOOL_GUARD_FIELD = "Tool guard, in uses left";
export const TOOL_GUARD_HINT = "Mining stops when a tool has this many uses left. 0 turns it off.";
export const BLOCKS_HEADING = "Blocks it mines";
export const BLOCKS_HINT =
  "A held left button mines only these, such as cobblestone. None listed: any block.";
export const ADD_BLOCK = "Add block";
export function blockLabel(n: number): string {
  return `Block ${n}`;
}
export function removeBlock(n: number): string {
  return `Remove block ${n}`;
}

/** The declared messages. */
export const MESSAGES_HEADING = "Messages";
export const MESSAGES_HINT =
  'What the script may send, by its key: chat.send("key", #{ name: value }). A {name} in the text is a value the script fills, in its range.';
export const ADD_MESSAGE = "Add message";
export function messageLabel(n: number): string {
  return `Message ${n}`;
}
export function removeMessage(n: number): string {
  return `Remove message ${n}`;
}
export const KEY_FIELD = "Key";
export const MESSAGE_TEXT_FIELD = "Text";
export const VALUES_HEADING = "Values";
export const ADD_VALUE = "Add value";
export function valueLabel(n: number): string {
  return `Value ${n}`;
}
export function removeValue(n: number): string {
  return `Remove value ${n}`;
}
/** A value's name, as the text writes it in braces. */
export const NAME_OF_VALUE = "Name in the text";
/** A capture's name, as the line writes it in braces. */
export const NAME_OF_CAPTURE = "Name in the line";
export const TYPE_FIELD = "Type";
export const VALUE_TYPE_TEXT = {
  integer: "Whole number",
  decimal: "Decimal",
  word: "One of some words",
} as const;
export const MIN_FIELD = "From";
export const MAX_FIELD = "To";
export const PLACES_FIELD = "Decimals";
export const WORDS_FIELD = "Words";
export const WORDS_HINT = "Separated by spaces.";

/** The declared signs (API level 2; ADR-0036, M16). */
export const SIGNS_HEADING = "Signs";
/** Under the signs' heading: what a sign is for, and its level. */
export const SIGNS_HINT =
  'What the script may write on a sign the server opens, by its key: sign.write("key", #{ name: value }). Up to four lines; a line may start with a value, or be empty. Needs agents of this release or newer.';
/** The button that adds a sign. */
export const ADD_SIGN = "Add sign";
/** A sign's group, from 1. */
export function signLabel(n: number): string {
  return `Sign ${n}`;
}
/** The button that removes sign `n`. */
export function removeSign(n: number): string {
  return `Remove sign ${n}`;
}
/** A sign's line `n`, from 1. */
export function signLineField(n: number): string {
  return `Line ${n}`;
}

/** The declared chat patterns (ADR-0037). */
export const PATTERNS_HEADING = "Chat patterns";
export const PATTERNS_HINT =
  "Lines of the server's own chat the script reads, whole, in on_chat(key, values). Never a player's line.";
export const ADD_PATTERN = "Add pattern";
export function patternLabel(n: number): string {
  return `Pattern ${n}`;
}
export function removePattern(n: number): string {
  return `Remove pattern ${n}`;
}
export const LINE_FIELD = "Line";
export const WHEN_FIELD = "When it counts";
export const WHEN_TEXT = {
  reply: "As a reply, after chat.expect",
  any_time: "At any time",
} as const;
/** The warning at a pattern that may come at any time (ADR-0037, C5). */
export const ANY_TIME_WARNING =
  "Riskier: any line the server prints that fits counts, not only a reply to the script's own command. Prefer a reply.";
export const CAPTURES_HEADING = "Captures";
export const ADD_CAPTURE = "Add capture";
export function captureLabel(n: number): string {
  return `Capture ${n}`;
}
export function removeCapture(n: number): string {
  return `Remove capture ${n}`;
}
export const CAPTURE_TYPE_TEXT = {
  integer: "Whole number",
  amount: "Amount, such as 1,234.5 or 1.5k",
  word: "One of some words",
} as const;
export const DECIMAL_MARK_FIELD = "Decimal mark";
export const DECIMAL_MARK_TEXT = { ".": "Point: 1,234.56", ",": "Comma: 1.234,56" } as const;
/** Next to an Add button once the most are there. */
export function atMost(count: number, what: string): string {
  return `At most ${count} ${what}.`;
}
/** A one-line text's error with the character it's at. */
export function atCharacter(message: string, column: number): string {
  return `${message} (at character ${column})`;
}
/** A mode whose script reads chat at any time, on its card and its screen (ADR-0037, C5). */
export const MARK_ANY_TIME_CHAT = "Reads the server's chat at any time.";

// --- Convert to script (Plan.md P15.27; ADR-0046) ----------------------------------------------

/** The way from steps to a script, for whoever writes scripts. */
export const CONVERT = "Convert to script";
export const CONVERT_HINT =
  "Writes these steps as a script that does the same. Nothing is saved until you save.";
/** The notice after a conversion. */
export const CONVERTED_HEADING = "Converted to a script";
export const CONVERTED_SAVE =
  "Check the script and what's different, then save to keep it. Until then the mode keeps its steps.";
/** What every script does differently from steps, whatever they were. */
export const CONVERTED_EVERY_SCRIPT =
  "In every script, the bot lets go of every key and button when it dies, respawns or changes worlds, and its timers wait while it's dead.";
export const CONVERTED_NOTHING_ELSE = "Nothing else differs.";
export const KEEP_THE_STEPS = "Keep the steps";

/** A step as the notes name it: its section's heading and its number there. */
export function stepPlace(heading: string, n: number): string {
  return `${heading}, step ${n}`;
}

/** Milliseconds as seconds: 0.5 s, 30 s. */
function seconds(ms: number): string {
  return `${ms / 1000} s`;
}

/** What a converted step does differently, after its place (`where`). */
export const NOTE_TEXT = {
  swing_skips_entities: () =>
    "swings only while the crosshair isn't on an entity. A script swings with a left click, which would hit one.",
  slower: (everyMs: number) =>
    `runs at most every ${seconds(everyMs)}, a script's shortest for it.`,
  hold_repeats: () =>
    "holds the right button, which uses an item that isn't used over time again every 4 ticks.",
  gap_capped: () => "waits at most 24 hours between two runs, a timer's longest.",
  shares_limit: (everyMs: number, other: string) =>
    `shares a script's limit of once every ${seconds(everyMs)} with ${other}: a run that close to one of its runs is skipped.`,
} as const;
