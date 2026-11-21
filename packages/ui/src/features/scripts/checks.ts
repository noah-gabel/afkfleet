// The editor's marks while a script is typed (Plan.md P15.25; ADR-0045). Three findings, one lint
// source, run by CodeMirror's linter after the typing pauses:
// - the app's check (`checkScript`): the Rust side compiles the text with the engine the server
//   runs, off the UI's thread, and answers the first problem the server would, at its line and
//   column; a check that fails is "couldn't check", never a mark and never a crash: the text may
//   be another Admin's shared script;
// - what the compiler can't see, from the description (`memberProblems`);
// - the server's own answer at the last save (`setServerProblem`), until the text changes. The
//   server stays the judge.
// The check's outcome goes to `report`, which the editor shows under itself.
import { type Diagnostic, linter } from "@codemirror/lint";
import { type EditorState, StateEffect, StateField, type Text } from "@codemirror/state";
import type { EditorView } from "@codemirror/view";

import { toApiError } from "../../api/ApiError";
import { describeError } from "../../api/describeError";
import type { FieldError } from "../../generated/FieldError";
import type { ScriptCheck } from "../../generated/ScriptCheck";
import { type Declared, memberProblems } from "./members";
import { markAt, offsetOf, placeOf } from "./places";

/** How long the typing pauses before a check, in milliseconds. */
export const CHECK_DELAY_MS = 500;

/** A mark as the editor lists it: its place as the engine counts it, and its message. */
export interface Mark {
  readonly line: number;
  readonly column: number;
  readonly message: string;
}

/** Where the check stands, as the editor shows it. */
export type CheckStatus =
  | { readonly kind: "checking" }
  | { readonly kind: "done"; readonly marks: readonly Mark[] }
  | { readonly kind: "failed"; readonly reason: string; readonly marks: readonly Mark[] };

/** `marks` on `doc` as the editor lists them, in the text's order. */
function listed(doc: Text, marks: readonly Diagnostic[]): Mark[] {
  return [...marks]
    .sort((a, b) => a.from - b.from)
    .map((mark) => ({ ...placeOf(doc, mark.from), message: mark.message }));
}

/** What the lint source needs from outside the editor. */
export interface CheckDeps {
  /** The app's check of a script's text. */
  readonly check: (source: string) => Promise<ScriptCheck>;
  /** What the mode declares now, if the form says. */
  readonly declared: () => Declared | undefined;
  /** The check's outcome. */
  readonly report: (status: CheckStatus) => void;
}

/** The server's problem at the last save, or none. */
export const setServerProblem = StateEffect.define<FieldError | null>();

/** The server's problem at the last save, until the text changes. */
export const serverProblem = StateField.define<FieldError | null>({
  create: () => null,
  update(value, transaction) {
    for (const effect of transaction.effects) {
      if (effect.is(setServerProblem)) {
        return effect.value;
      }
    }
    return transaction.docChanged ? null : value;
  },
});

/** `problem` as a mark: at its place, or at the text's start without one. */
export function diagnosticOf(doc: Text, problem: FieldError, source: string): Diagnostic {
  const at = problem.line === undefined ? 0 : offsetOf(doc, problem.line, problem.column ?? 1);
  return { ...markAt(doc, at), severity: "error", message: problem.message, source };
}

/** Every finding on `state`'s text, the app's check's answer `checked` among them. */
function findings(
  state: EditorState,
  deps: CheckDeps,
  checked: FieldError | undefined,
): Diagnostic[] {
  const doc = state.doc;
  const marks: Diagnostic[] = [];
  const server = state.field(serverProblem, false) ?? null;
  if (server !== null) {
    marks.push(diagnosticOf(doc, server, "server"));
  }
  if (checked !== undefined) {
    const mark = diagnosticOf(doc, checked, "check");
    if (!marks.some((one) => one.from === mark.from && one.message === mark.message)) {
      marks.push(mark);
    }
  }
  for (const problem of memberProblems(doc.toString(), deps.declared())) {
    marks.push({ ...problem, severity: "error", source: "api" });
  }
  return marks;
}

/** The lint source; see the module comment. */
export function checkSource(deps: CheckDeps): (view: EditorView) => Promise<Diagnostic[]> {
  return async (view) => {
    const state = view.state;
    deps.report({ kind: "checking" });
    try {
      const answer = await deps.check(state.doc.toString());
      const marks = findings(state, deps, answer.problem);
      deps.report({ kind: "done", marks: listed(state.doc, marks) });
      return marks;
    } catch (rejection) {
      const marks = findings(state, deps, undefined);
      deps.report({
        kind: "failed",
        reason: describeError(toApiError(rejection).error),
        marks: listed(state.doc, marks),
      });
      return marks;
    }
  };
}

/** The linter that runs the source after the typing pauses, with the server's field. */
export function scriptChecks(deps: CheckDeps) {
  return [
    serverProblem,
    linter(checkSource(deps), {
      delay: CHECK_DELAY_MS,
      // The server's answer shows at once, without waiting for the typing.
      needsRefresh: (update) =>
        update.transactions.some((transaction) =>
          transaction.effects.some((effect) => effect.is(setServerProblem)),
        ),
    }),
  ];
}
