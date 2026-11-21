// What the editor does (Plan.md P15.24, P15.25; ADR-0045): CodeMirror's usual editing set, picked
// by hand rather than through the `codemirror` package's `basicSetup`, so every part is named here.
// - Line numbers, the active line, special characters shown, folding by braces.
// - Undo and redo; bracket matching and closing; indentation as Rhai's brackets nest, and again
//   when a closing bracket is typed.
// - Search and replace (Ctrl+F; the panel on top); several cursors (Ctrl+click, Alt+drag for a
//   rectangle, Ctrl+D for the next match) and the selection's other matches marked.
// - Tab indents and Shift+Tab takes it back. **Esc, then Tab, leaves the editor:** CodeMirror lets
//   Tab move the focus for two seconds after Esc.
// - Completion, hover help and the marks, from the generated description and the app's check.
import {
  autocompletion,
  closeBrackets,
  closeBracketsKeymap,
  completionKeymap,
} from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import {
  bracketMatching,
  foldGutter,
  foldKeymap,
  indentOnInput,
  indentUnit,
} from "@codemirror/language";
import { lintKeymap } from "@codemirror/lint";
import { highlightSelectionMatches, search, searchKeymap } from "@codemirror/search";
import { EditorState, type Extension } from "@codemirror/state";
import {
  crosshairCursor,
  drawSelection,
  dropCursor,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  rectangularSelection,
} from "@codemirror/view";

import { type CheckDeps, scriptChecks } from "./checks";
import { scriptCompletions } from "./complete";
import { scriptHover } from "./hover";
import { braceFold, rhaiLanguage } from "./rhai";
import { KEYBOARD_HINT } from "./texts";
import { scriptHighlighting, scriptTheme } from "./theme";

/** The editor's indentation: four spaces, as the reference's examples are written. */
export const INDENT = "    ";

/** Every extension of an editor that checks through `deps`, labelled `label`. */
export function scriptExtensions(deps: CheckDeps, label: string): Extension[] {
  return [
    lineNumbers(),
    highlightActiveLineGutter(),
    highlightSpecialChars(),
    history(),
    foldGutter(),
    braceFold,
    drawSelection(),
    dropCursor(),
    EditorState.allowMultipleSelections.of(true),
    indentUnit.of(INDENT),
    indentOnInput(),
    rhaiLanguage,
    scriptHighlighting,
    bracketMatching(),
    closeBrackets(),
    autocompletion({ override: [scriptCompletions], icons: false }),
    rectangularSelection(),
    crosshairCursor(),
    highlightActiveLine(),
    highlightSelectionMatches(),
    search({ top: true }),
    scriptHover,
    scriptChecks(deps),
    keymap.of([
      ...closeBracketsKeymap,
      ...defaultKeymap,
      ...searchKeymap,
      ...historyKeymap,
      ...foldKeymap,
      ...completionKeymap,
      ...lintKeymap,
      indentWithTab,
    ]),
    // ARIA's ids don't cross the shadow root, so the hint under the editor is said here too.
    EditorView.contentAttributes.of({ "aria-label": label, "aria-description": KEYBOARD_HINT }),
    scriptTheme,
  ];
}
