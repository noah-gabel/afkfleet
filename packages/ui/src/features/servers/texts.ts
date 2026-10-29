// The Minecraft-servers screens' texts (Plan.md P11.9; ADR-0020's texts table, U57, the
// user's hint): the list's and the editor's. A server shows as its key, the host and a port
// that's always written, as the server answers it.
import { CHAT_FORMATS_MAX, CONFLICT_TEXTS_MAX, SERVER_SETTINGS_MAX } from "../../api/limits";

/** The nav entry's and the list's heading. */
export const SERVERS_HEADING = "Minecraft servers";
/** What the list says first. */
export const SERVERS_INTRO =
  "Settings here apply to every bot on that server, whoever's bot it is.";
/** The list's way to a new entry, and the new entry's heading. */
export const ADD_SERVER = "Add a server";
/** The list without an entry. */
export const NO_SERVERS = "No server has settings yet.";
/** The list, or an entry, while the servers load. */
export const LOADING_SERVERS = "Loading servers…";

/** The list, or an entry, when the servers couldn't be read. */
export function serversFailed(why: string): string {
  return `Couldn't load the servers: ${why}`;
}

/** "Load again" when the read was paused, so nothing was read (PR #81's review, c). */
export const RELOAD_PAUSED = serversFailed(
  "Windows reports no network connection, so the app didn't ask the server. Try again.",
);

/** An entry's link, for screen readers. */
export function openServer(server: string): string {
  return `Open ${server}`;
}

/** How many conflict texts an entry holds. */
export function conflictTextCount(count: number): string {
  return count === 1 ? "1 conflict text" : `${count} conflict texts`;
}

/** How many chat formats an entry holds. */
export function chatFormatCount(count: number): string {
  return count === 1 ? "1 chat format" : `${count} chat formats`;
}

/**
 * What an entry holds, only the parts it has (the user's change at H7's plan review): its
 * texts, its formats, or both joined by " · ". A row always holds one of them.
 */
export function settingsCount(texts: number, formats: number): string {
  const parts = [];
  if (texts > 0 || formats === 0) {
    parts.push(conflictTextCount(texts));
  }
  if (formats > 0) {
    parts.push(chatFormatCount(formats));
  }
  return parts.join(" · ");
}

/** The editor's field and section. */
export const SERVER_FIELD = "Server address";
export const SERVER_HINT = "Host or host:port, e.g. play.example.com";
export const TEXTS_HEADING = "Conflict texts";
/** The user's hint: where a message comes from, and what " | " means (ADR-0020). */
export const TEXTS_HINT =
  'When a bot is kicked with exactly one of these messages, it counts as someone logging in with the bot\'s account: the bot pauses instead of reconnecting, so it doesn\'t kick them. Copy the message from the bot\'s Last disconnect, after "Kicked: ". Where it shows " | ", the message has a line break: press Enter there instead.';

/** The label of text `n`, counted from 1. */
export function conflictTextLabel(n: number): string {
  return `Conflict text ${n}`;
}

export const REMOVE = "Remove";
export const ADD_TEXT = "Add text";
/** Next to "Add text" once the most texts are there. */
export const AT_MOST_TEXTS = `At most ${CONFLICT_TEXTS_MAX} texts.`;
export const FORMATS_HEADING = "Chat formats";
/** What a format does and how to write one (ADR-0020's table, H7). */
export const FORMATS_HINT =
  "Some servers send players' chat as server text, like \"[Clan] Name : hello\". A format finds the player's name in such a line, for display only. Write the line's start as the server sends it, with {sender} where the name is and {any} for a part that changes, such as a clan tag. The first format that fits names the line.";

/** The label of format `n`, counted from 1. */
export function chatFormatLabel(n: number): string {
  return `Chat format ${n}`;
}

export const ADD_FORMAT = "Add format";
/** Next to "Add format" once the most formats are there. */
export const AT_MOST_FORMATS = `At most ${CHAT_FORMATS_MAX} formats.`;
/** The preview's field, its placeholder and its button (U58, R148). */
export const TRY_LINE = "Try a line";
export const TRY_LINE_PLACEHOLDER = "Paste a line from the server's chat";
export const TRY = "Try";

/** The preview's answer: format `n`, counted from 1, read `name`. */
export function formatReads(n: number, name: string): string {
  return `Format ${n} reads the sender as ${name}.`;
}

export const NO_FORMAT_FITS = "No format fits this line.";
export const SAVE = "Save";
export const CANCEL = "Cancel";
export const DELETE = "Delete";
/** An entry that isn't there: never was, or went while its screen was open. */
export const SETTINGS_GONE = "These settings don't exist any more.";
/** After a 409 `server_settings_changed`, next to the server's own text. */
export const LOAD_AGAIN = "Load again";
/** A create's 409 `conflict`: the fleet holds the most servers' settings. */
export const CAP_REACHED = `There are settings for ${SERVER_SETTINGS_MAX} servers, the most there can be. Delete a server's settings to add another.`;

/** The toast after a create. */
export function added(server: string): string {
  return `Added settings for ${server}.`;
}

/** The toast after a save. */
export const SAVED = "Saved. Bots on this server use the change at once.";

/** What Delete asks first. */
export function deleteQuestion(server: string): string {
  return `Delete the settings of ${server}? Its bots then treat no kick message as someone logging in.`;
}

export const DELETE_SETTINGS = "Delete settings";

/** The toast after a delete. */
export function deleted(server: string): string {
  return `Deleted the settings of ${server}.`;
}
