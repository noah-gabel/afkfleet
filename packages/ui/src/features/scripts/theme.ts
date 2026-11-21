// The editor's look (Plan.md P15.24; ADR-0036, D16; ADR-0045): every colour one of the app's tokens
// (styles.css), never a literal, so the light and dark themes and the contrast sweep hold here as
// everywhere. CustomProperties cross the shadow root, so `var(--…)` follows the app's theme.
//
// CodeMirror's packages bring base themes with colours of their own (the gutters, the selection,
// the search panel, the completion list, the lint marks and tooltips, matched brackets, fold
// markers); each is set again here, at the same weight and later, so none of theirs shows. The lint
// marks are a wavy underline, not lint's `data:` images. The editor draws no outline of its own:
// its host shows the focus (styles.css's one focus rule).
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { EditorView } from "@codemirror/view";
import { tags } from "@lezer/highlight";

/** A token mixed into nothing, `percent` strong. */
function tint(token: string, percent: number): string {
  return `color-mix(in srgb, var(${token}) ${percent}%, transparent)`;
}

/** The editor's own parts. */
export const scriptTheme = EditorView.theme({
  "&": {
    color: "var(--foreground)",
    backgroundColor: "transparent",
    fontSize: "0.875rem",
  },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": { fontFamily: "var(--font-mono)", lineHeight: "1.5" },
  ".cm-content": { caretColor: "var(--foreground)", minHeight: "14rem", padding: "0.5rem 0" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--foreground)" },
  "&.cm-focused > .cm-scroller > .cm-cursorLayer .cm-cursor": {
    borderLeftColor: "var(--foreground)",
  },
  ".cm-selectionBackground": { background: tint("--primary", 22) },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground": {
    background: tint("--primary", 28),
  },
  ".cm-activeLine": { backgroundColor: tint("--accent", 45) },
  ".cm-activeLineGutter": { backgroundColor: tint("--accent", 45), color: "var(--foreground)" },
  ".cm-gutters": {
    backgroundColor: "transparent",
    color: "var(--muted-foreground)",
    border: "none",
    borderRight: "1px solid var(--border)",
  },
  ".cm-lineNumbers .cm-gutterElement": { padding: "0 0.5rem 0 0.75rem" },
  ".cm-foldGutter .cm-gutterElement": { padding: "0 0.25rem", color: "var(--muted-foreground)" },
  ".cm-foldPlaceholder": {
    backgroundColor: "var(--muted)",
    border: "1px solid var(--border)",
    color: "var(--foreground)",
  },
  ".cm-specialChar": { color: "var(--destructive)" },
  ".cm-placeholder": { color: "var(--muted-foreground)" },
  ".cm-matchingBracket, &.cm-focused .cm-matchingBracket": {
    backgroundColor: tint("--success", 18),
    textDecoration: "underline",
    textDecorationColor: "var(--success)",
  },
  ".cm-nonmatchingBracket, &.cm-focused .cm-nonmatchingBracket": {
    backgroundColor: tint("--destructive", 18),
  },
  ".cm-selectionMatch": { backgroundColor: tint("--info", 16) },
  ".cm-searchMatch": { backgroundColor: tint("--warning", 22), outline: "none" },
  ".cm-searchMatch.cm-searchMatch-selected": { backgroundColor: tint("--warning", 40) },
  ".cm-searchMatch .cm-selectionMatch": { backgroundColor: "transparent" },
  // The search and replace panel.
  ".cm-panels": {
    backgroundColor: "var(--popover)",
    color: "var(--popover-foreground)",
  },
  ".cm-panels.cm-panels-top": { borderBottom: "1px solid var(--border)" },
  ".cm-panels.cm-panels-bottom": { borderTop: "1px solid var(--border)" },
  ".cm-panel.cm-search": { padding: "0.5rem 2rem 0.5rem 0.5rem", fontFamily: "var(--font-sans)" },
  ".cm-panel.cm-search label": { fontSize: "0.8125rem" },
  ".cm-textfield": {
    backgroundColor: "var(--background)",
    color: "var(--foreground)",
    border: "1px solid var(--input)",
    borderRadius: "var(--radius)",
    fontSize: "0.8125rem",
    padding: "0.125rem 0.375rem",
  },
  ".cm-button": {
    backgroundImage: "none",
    backgroundColor: "var(--secondary)",
    color: "var(--secondary-foreground)",
    border: "1px solid var(--border)",
    borderRadius: "var(--radius)",
    fontSize: "0.8125rem",
    padding: "0.125rem 0.625rem",
  },
  ".cm-button:active": { backgroundImage: "none", backgroundColor: "var(--accent)" },
  ".cm-panel button[name=close]": {
    color: "var(--muted-foreground)",
    backgroundColor: "transparent",
    fontSize: "1rem",
  },
  // Tooltips: hover help, the completion list, a mark's message.
  ".cm-tooltip": {
    backgroundColor: "var(--popover)",
    color: "var(--popover-foreground)",
    border: "1px solid var(--border)",
    borderRadius: "var(--radius)",
  },
  ".cm-tooltip .cm-tooltip-arrow:before": { borderTopColor: "var(--border)" },
  ".cm-tooltip .cm-tooltip-arrow:after": { borderTopColor: "var(--popover)" },
  ".cm-tooltip.cm-tooltip-autocomplete > ul": { fontFamily: "var(--font-mono)" },
  ".cm-tooltip.cm-tooltip-autocomplete > ul > li": { padding: "0.125rem 0.5rem" },
  ".cm-tooltip-autocomplete ul li[aria-selected]": {
    background: "var(--primary)",
    color: "var(--primary-foreground)",
  },
  ".cm-tooltip-autocomplete-disabled ul li[aria-selected]": {
    background: "var(--muted)",
    color: "var(--foreground)",
  },
  ".cm-completionDetail": { color: "var(--muted-foreground)", fontStyle: "normal" },
  ".cm-tooltip-autocomplete ul li[aria-selected] .cm-completionDetail": { color: "inherit" },
  ".cm-completionMatchedText": { textDecoration: "underline" },
  ".cm-completionInfo": { padding: "0.375rem 0.625rem", maxWidth: "24rem" },
  ".cm-script-help": { padding: "0.375rem 0.625rem", maxWidth: "28rem" },
  ".cm-script-help code": { fontFamily: "var(--font-mono)", fontWeight: "600" },
  ".cm-script-help p": { margin: "0.25rem 0 0", fontFamily: "var(--font-sans)" },
  // A mark: a wavy underline in the token's colour, and its message in a tooltip.
  ".cm-lintRange-error": {
    backgroundImage: "none",
    textDecoration: "underline wavy var(--destructive)",
    textUnderlineOffset: "0.2em",
  },
  ".cm-lintRange-warning": {
    backgroundImage: "none",
    textDecoration: "underline wavy var(--warning)",
    textUnderlineOffset: "0.2em",
  },
  ".cm-lintRange-active": { backgroundColor: tint("--destructive", 12) },
  ".cm-diagnostic": { padding: "0.375rem 0.625rem", fontFamily: "var(--font-sans)" },
  ".cm-diagnostic-error": { borderLeft: "4px solid var(--destructive)" },
  ".cm-diagnostic-warning": { borderLeft: "4px solid var(--warning)" },
  ".cm-diagnostic-info": { borderLeft: "4px solid var(--info)" },
  ".cm-tooltip-lint": { backgroundColor: "var(--popover)", color: "var(--popover-foreground)" },
  ".cm-panel.cm-panel-lint ul [aria-selected]": {
    backgroundColor: "var(--primary)",
    color: "var(--primary-foreground)",
  },
  ".cm-snippetField": { backgroundColor: tint("--info", 16) },
  ".cm-snippetFieldPosition": { borderLeft: "1px dotted var(--muted-foreground)" },
});

/** The highlighting, every colour a token that holds 4.5:1 on the app's surfaces. */
export const scriptHighlighting = syntaxHighlighting(
  HighlightStyle.define([
    { tag: tags.keyword, color: "var(--primary)", fontWeight: "600" },
    { tag: [tags.bool, tags.number], color: "var(--warning)" },
    { tag: tags.string, color: "var(--success)" },
    { tag: tags.comment, color: "var(--muted-foreground)", fontStyle: "italic" },
    { tag: tags.special(tags.variableName), color: "var(--primary)" },
    { tag: tags.function(tags.definition(tags.variableName)), fontWeight: "600" },
    { tag: tags.invalid, color: "var(--destructive)", textDecoration: "underline" },
  ]),
);
