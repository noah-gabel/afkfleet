// The lines a bot's chat remembers, and browsing them with ↑ and ↓ (the user's addition to P15.26;
// ADR-0045). Pure.
//
// - **What's kept:** per bot, only the lines the server took, at most `CHAT_RECALL_MAX`, the same
//   line twice in a row once. A line can hold a password (ADR-0020, U34), so they live in memory
//   alone (`stores.chatRecall`): no cache, no storage, nothing on disk; the session's end, a
//   sign-out and the bot's removal drop them.
// - **Browsing:** ↑ fills in the newest line, ↑ again the one before it, and keeps what was typed
//   when browsing began; ↓ goes forward again, and past the newest shows what was typed.
//   Typing ends browsing.

/** The most lines a bot's chat remembers. */
export const CHAT_RECALL_MAX = 50;

/** `lines` with `line` last: the same line twice in a row once, the newest kept. */
export function remembered(lines: readonly string[], line: string): readonly string[] {
  if (lines.at(-1) === line) {
    return lines;
  }
  return [...lines, line].slice(-CHAT_RECALL_MAX);
}

/** Where browsing stands: `at` lines back from the newest (0 is the newest), and what was typed. */
export interface Browse {
  readonly at: number | null;
  readonly draft: string;
}

/** Not browsing. */
export const NOT_BROWSING: Browse = { at: null, draft: "" };

/** A step of browsing: where it stands now, and what the box shows. */
export interface Step {
  readonly browse: Browse;
  readonly text: string;
}

/** The line `at` back from the newest. */
function lineAt(lines: readonly string[], at: number): string {
  return lines[lines.length - 1 - at] ?? "";
}

/** ↑: the line before, or the newest when not browsing, keeping `current`; null at the oldest. */
export function older(lines: readonly string[], browse: Browse, current: string): Step | null {
  const at = browse.at === null ? 0 : browse.at + 1;
  if (at >= lines.length) {
    return null;
  }
  const draft = browse.at === null ? current : browse.draft;
  return { browse: { at, draft }, text: lineAt(lines, at) };
}

/** ↓: the line after, or what was typed past the newest; null when not browsing. */
export function newer(lines: readonly string[], browse: Browse): Step | null {
  if (browse.at === null) {
    return null;
  }
  if (browse.at === 0) {
    return { browse: NOT_BROWSING, text: browse.draft };
  }
  const at = browse.at - 1;
  return { browse: { at, draft: browse.draft }, text: lineAt(lines, at) };
}
