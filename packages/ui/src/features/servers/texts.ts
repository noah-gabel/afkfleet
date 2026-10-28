// The Minecraft-servers screens' texts (Plan.md P11.9; ADR-0020's texts table, U57, the
// user's hint): the list's and the editor's. A server shows as its key, the host and a port
// that's always written, as the server answers it.
import { CONFLICT_TEXTS_MAX, SERVER_SETTINGS_MAX } from "../../api/limits";

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

/** An entry's link, for screen readers. */
export function openServer(server: string): string {
  return `Open ${server}`;
}

/** How many conflict texts an entry holds. */
export function conflictTextCount(count: number): string {
  return count === 1 ? "1 conflict text" : `${count} conflict texts`;
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
