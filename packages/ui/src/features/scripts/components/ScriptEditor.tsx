// The script editor (Plan.md P15.24, P15.25; ADR-0036, D16; ADR-0045): CodeMirror 6, mounted in a
// shadow root of its host.
//
// - **Why a shadow root:** CodeMirror's styles come from style-mod, which writes a `<style>` element
//   into a document (refused by the release CSP's `style-src 'self'`) and a constructed stylesheet
//   into a shadow root, which Edge's CSP allows. The constructed stylesheet in this shadow root is
//   frontend.md's one exception to "no runtime styles"; the document holds none, and the style
//   guard checks both. CustomProperties cross the shadow root, so the theme's tokens follow the app.
// - **The host** sits in the document: its border, corner and fixed height are the app's classes
//   (26rem, which an 800×600 window shows whole, so its focus outline is always in sight), the
//   editor scrolls inside it, and the one focus rule draws its outline while the editor has the focus.
// - **The text** starts from `value`, and a different `value` from outside (a template, a reload)
//   replaces it; every change goes to `onChange`. `readOnly` keeps it focusable, searchable and
//   copyable, never changed.
// - **The marks** come from the app's check, the description and `serverProblem`, the server's
//   answer at the last save (checks.ts). Under the editor, the check's state and every mark, each
//   a button that moves the cursor to it; a check that failed says so, never more.
// - **`focusAt`** moves the cursor to a place and shows it: a script's error on the bot's page links
//   here at its line.
import { Compartment, EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { useCallback, useEffect, useRef, useState } from "react";

import { useApiClient } from "../../../api/context";
import type { FieldError } from "../../../generated/FieldError";
import type { ScriptNeed } from "../../../generated/ScriptNeed";
import type { CheckDeps, CheckStatus, Mark } from "../checks";
import { setServerProblem } from "../checks";
import type { Declared } from "../members";
import { offsetOf } from "../places";
import { scriptExtensions } from "../setup";
import {
  couldntCheck,
  DECLARES_SIGNS,
  KEYBOARD_HINT,
  levelLine,
  NEED_TEXT,
  NO_PROBLEMS,
  problemAt,
  problemCount,
} from "../texts";

/** A place in the text, from 1, as the engine counts. */
export interface Place {
  readonly line: number;
  readonly column?: number;
}

/** The editor's props; see the module comment. */
export interface ScriptEditorProps {
  readonly value: string;
  readonly onChange?: (value: string) => void;
  readonly readOnly?: boolean;
  /** Its accessible name. */
  readonly label: string;
  readonly serverProblem?: FieldError | null;
  readonly declared?: Declared;
  readonly focusAt?: Place | null;
}

/** The marks the editor lists, as buttons that move the cursor. */
function MarkList({ marks, onPick }: { marks: readonly Mark[]; onPick: (mark: Mark) => void }) {
  return (
    <ul className="flex flex-col gap-1">
      {marks.map((mark) => (
        <li key={`${mark.line}:${mark.column}:${mark.message}`}>
          <button
            type="button"
            className="text-left text-sm text-destructive underline-offset-2 hover:underline"
            onClick={() => onPick(mark)}
          >
            {problemAt(mark.line, mark.column, mark.message)}
          </button>
        </li>
      ))}
    </ul>
  );
}

/**
 * The level the server would stamp the script with, and why, in words: what the check found in
 * the text, and the mode's declared signs (Plan.md P16.15). None while the text has a problem.
 */
function Level({
  level,
  needs,
  signs,
}: {
  level: number;
  needs: readonly ScriptNeed[];
  signs: boolean;
}) {
  if (level === 0) {
    return null;
  }
  const reasons = [
    ...needs.map((need) => NEED_TEXT[need] ?? need),
    ...(signs ? [DECLARES_SIGNS] : []),
  ];
  return (
    <p className="text-sm text-muted-foreground" data-testid="script-level">
      {levelLine(signs ? 2 : level, reasons)}
    </p>
  );
}

/** What the check says under the editor; `signs` says whether the mode declares signs. */
function Status({
  status,
  signs,
  onPick,
}: {
  status: CheckStatus | null;
  signs: boolean;
  onPick: (mark: Mark) => void;
}) {
  if (status === null) {
    return null;
  }
  switch (status.kind) {
    case "done":
      return status.marks.length === 0 ? (
        <>
          <p className="text-sm text-muted-foreground">{NO_PROBLEMS}</p>
          <Level level={status.level} needs={status.needs} signs={signs} />
        </>
      ) : (
        <>
          <p className="text-sm text-destructive">{problemCount(status.marks.length)}</p>
          <MarkList marks={status.marks} onPick={onPick} />
          <Level level={status.level} needs={status.needs} signs={signs} />
        </>
      );
    case "failed":
      return (
        <>
          <p className="text-sm text-muted-foreground">{couldntCheck(status.reason)}</p>
          <MarkList marks={status.marks} onPick={onPick} />
        </>
      );
    default:
      return null;
  }
}

/** The script editor; see the module comment. */
export function ScriptEditor(props: ScriptEditorProps) {
  const { value, readOnly = false, serverProblem = null, focusAt = null } = props;
  const api = useApiClient();
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const latest = useRef(props);
  latest.current = props;
  const readOnlyRoom = useRef(new Compartment());
  const [status, setStatus] = useState<CheckStatus | null>(null);

  // Mounted once per client: the text and the flags follow through the effects below.
  useEffect(() => {
    const element = host.current;
    if (element === null) {
      return;
    }
    let mounted = true;
    // A second mount (React's strict mode) finds the root the first one attached.
    const root = element.shadowRoot ?? element.attachShadow({ mode: "open" });
    const deps: CheckDeps = {
      check: (source) => api.checkScript({ source }),
      declared: () => latest.current.declared,
      report: (next) => {
        // The last answer stays until the next one: "checking" would flicker as one types.
        if (mounted && next.kind !== "checking") {
          setStatus(next);
        }
      },
    };
    const editor = new EditorView({
      root,
      parent: root,
      state: EditorState.create({
        doc: latest.current.value,
        extensions: [
          scriptExtensions(deps, latest.current.label),
          readOnlyRoom.current.of(EditorState.readOnly.of(latest.current.readOnly === true)),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) {
              latest.current.onChange?.(update.state.doc.toString());
            }
          }),
        ],
      }),
    });
    view.current = editor;
    return () => {
      mounted = false;
      view.current = null;
      editor.destroy();
    };
  }, [api]);

  useEffect(() => {
    const editor = view.current;
    if (editor !== null && editor.state.doc.toString() !== value) {
      editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: value } });
    }
  }, [value]);

  useEffect(() => {
    view.current?.dispatch({
      effects: readOnlyRoom.current.reconfigure(EditorState.readOnly.of(readOnly)),
    });
  }, [readOnly]);

  useEffect(() => {
    view.current?.dispatch({ effects: setServerProblem.of(serverProblem) });
  }, [serverProblem]);

  /** Moves the cursor to `place` and shows it. */
  const goTo = useCallback((place: Place): void => {
    const editor = view.current;
    if (editor === null) {
      return;
    }
    const at = offsetOf(editor.state.doc, place.line, place.column ?? 1);
    editor.dispatch({ selection: { anchor: at }, scrollIntoView: true });
    editor.focus();
  }, []);

  const line = focusAt?.line;
  const column = focusAt?.column;
  useEffect(() => {
    if (line !== undefined) {
      goTo({ line, ...(column === undefined ? {} : { column }) });
    }
  }, [line, column, goTo]);

  return (
    <div className="flex min-w-0 flex-col gap-2">
      <div
        ref={host}
        data-slot="script-editor"
        className="h-[26rem] min-w-0 rounded-md border border-input bg-card"
      />
      <p className="text-sm text-muted-foreground">{KEYBOARD_HINT}</p>
      <div role="status" className="flex flex-col gap-1">
        <Status status={status} signs={(props.declared?.signs ?? []).length > 0} onPick={goTo} />
      </div>
    </div>
  );
}
