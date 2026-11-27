// The script editor's texts (Plan.md P15.24, P15.25; ADR-0045), in the app's terse voice.

/** The editor's accessible name when its field gives none. */
export const SCRIPT_LABEL = "Script";
/** Under the editor: how its keyboard works. */
export const KEYBOARD_HINT =
  "Tab indents. To leave the editor, press Esc, then Tab. Ctrl+Space suggests, Ctrl+F finds.";

/** A name a game object doesn't have: the compiler can't see it, the script stops there. */
export function unknownMember(chain: string, name: string): string {
  return `${chain} has no ${name}: the script would stop on this line.`;
}
/** A method written without its parentheses. */
export function notCalled(chain: string, signature: string): string {
  return `${chain} is a method: write ${signature}.`;
}
/** A value written as a method. */
export function notAMethod(chain: string): string {
  return `${chain} is a value, not a method: leave out the parentheses.`;
}
/** `chat.send` with a key the mode doesn't declare. */
export function undeclaredMessage(key: string): string {
  return `The mode declares no message "${key}": the script would stop on this line.`;
}
/** `chat.expect` with a key no reply pattern has. */
export function undeclaredReply(key: string): string {
  return `The mode declares no reply pattern "${key}": the script would stop on this line.`;
}

/** `sign.write` with a key the mode doesn't declare. */
export function undeclaredSign(key: string): string {
  return `The mode declares no sign "${key}": the script would stop on this line.`;
}

/** The check's state under the editor. */
export const NO_PROBLEMS = "No problems found.";
/** One problem, at its place when it has one. */
export function problemAt(
  line: number | undefined,
  column: number | undefined,
  message: string,
): string {
  if (line === undefined) {
    return message;
  }
  return column === undefined
    ? `Line ${line}: ${message}`
    : `Line ${line}, column ${column}: ${message}`;
}
/** How many problems the marks show. */
export function problemCount(count: number): string {
  return count === 1 ? "1 problem." : `${count} problems.`;
}
/** A check that failed says nothing about the script. */
export function couldntCheck(reason: string): string {
  return `Couldn't check the script: ${reason} The server checks it again at save.`;
}
/** Why a script needs script API level 2, in words (Plan.md P16.15). */
export const NEED_TEXT = {
  menus: "uses screens and the inventory",
  signs: "answers sign editors",
  clock: "uses the clock",
  bot_name: "reads the bot's name",
} as const;
/** A mode that declares signs needs level 2 whatever its text uses. */
export const DECLARES_SIGNS = "declares signs";
/** The level the server would stamp the script with, and why. */
export function levelLine(level: number, reasons: readonly string[]): string {
  return level < 2
    ? "Script API level 1: every agent that runs scripts runs it."
    : `Script API level 2 (${reasons.join(", ")}): only agents of this release or newer run it. A bot on an older agent waits until its agent is updated.`;
}

/** The server's own finding at save. */
export const SERVER_FOUND = "The server refused it at save:";
