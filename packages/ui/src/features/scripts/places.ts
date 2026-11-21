// A place in a script's text as the engine names it and as the editor holds it (Plan.md P15.25;
// ADR-0045). Pure.
//
// The engine names a line and a column from 1, counting characters (code points: a tab and an
// emoji are one each); CodeMirror counts UTF-16 units from 0. A place past a line's end is its
// end, and a line past the text's end is its last line, so a server's place in an older text
// still lands somewhere sensible.
import type { Text } from "@codemirror/state";

/** The offset in `doc` of `line` and `column`, each from 1; a column left out is the line's start. */
export function offsetOf(doc: Text, line: number, column = 1): number {
  const at = doc.line(Math.min(Math.max(1, Math.trunc(line)), doc.lines));
  let offset = 0;
  let counted = 1;
  for (const char of at.text) {
    if (counted >= column) {
      break;
    }
    offset += char.length;
    counted += 1;
  }
  return at.from + offset;
}

/** The line and the column, each from 1 and counted as the engine counts them, of `offset`. */
export function placeOf(doc: Text, offset: number): { line: number; column: number } {
  const clamped = Math.min(Math.max(0, offset), doc.length);
  const at = doc.lineAt(clamped);
  const before = at.text.slice(0, clamped - at.from);
  return { line: at.number, column: [...before].length + 1 };
}

/**
 * What a mark at `offset` covers: the name or number it's on, or one character, or, at a line's
 * end, the character before; nothing in an empty line.
 */
export function markAt(doc: Text, offset: number): { from: number; to: number } {
  const at = doc.lineAt(offset);
  const rest = at.text.slice(offset - at.from);
  const word = /^[\w$]+/.exec(rest)?.[0];
  if (word !== undefined) {
    return { from: offset, to: offset + word.length };
  }
  const char = [...rest][0];
  if (char !== undefined) {
    return { from: offset, to: offset + char.length };
  }
  const previous = [...at.text].at(-1);
  return previous === undefined
    ? { from: offset, to: offset }
    : { from: offset - previous.length, to: offset };
}
