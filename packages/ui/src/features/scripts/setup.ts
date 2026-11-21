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

/**
 * Typing over a selection, done by the editor itself (ADR-0045). Edge replaces selected text in
 * the DOM first, and where the selection spans highlighted tokens or lines it adds an inline-styled
 * span to keep their look, which the release CSP refuses (style-src-attr): the e2e story found it.
 * So a typed character over a selection, or at several cursors, goes through CodeMirror's own input
 * handlers (closing brackets still wrap the selection) and never reaches the DOM; and a read-only
 * text takes none. A plain cursor's typing stays the browser's, which input methods need.
 */
const typingOverSelection = EditorView.domEventHandlers({
  beforeinput(event, view) {
    if (event.inputType !== "insertText" || event.data === null) {
      return false;
    }
    const { state } = view;
    if (state.readOnly) {
      event.preventDefault();
      return true;
    }
    const { main, ranges } = state.selection;
    if (main.empty && ranges.length === 1) {
      return false;
    }
    event.preventDefault();
    const text = event.data;
    const insert = () =>
      state.update(state.replaceSelection(text), { userEvent: "input.type", scrollIntoView: true });
    const handled = state
      .facet(EditorView.inputHandler)
      .some((handler) => handler(view, main.from, main.to, text, insert));
    if (!handled) {
      view.dispatch(insert());
    }
    return true;
  },
});

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
    typingOverSelection,
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
    // Plain-text editing, as CodeMirror's base theme asks with `-webkit-user-modify`, which
    // Chromium no longer reads: with plain `true`, typing over a selection across highlighted
    // tokens made Edge add an inline-styled span, which the release CSP refused (style-src-attr),
    // found by the e2e story. CodeMirror only ever checks the attribute for "false".
    EditorView.contentAttributes.of({
      "aria-label": label,
      "aria-description": KEYBOARD_HINT,
      contenteditable: "plaintext-only",
    }),
    scriptTheme,
  ];
}
