// One mode's screen, and a new one's (Plan.md P11.9; ADR-0020, S14, S83, R160): the row comes from
// the every-page list, there being no single-mode route. An editable row gets the form, any other
// the locked view.
//
// "Make a copy" opens the mode's values here as a new mode, what they were when it was pressed
// (U63). They stay in this component's state alone, since a chat step can hold a password (R157).
//
// While the form is open, the list can change under it (S83):
// - a newer version changes nothing but the flags it offers by: Save's 409 says so;
// - a row it can no longer hold, one whose commands may be cut for this viewer, drops to the locked
//   view, the edits gone, so what the server just cut isn't kept on the screen, and that form
//   never comes back: only an editable row opens one again;
// - a row gone keeps the form, with its notice: a delete and a lost view look the same, and the
//   edits can still be saved as a copy. Only data actually read says a row is gone, never an
//   empty or reset query, so the form stays mounted through those.
import { useRef, useState } from "react";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { useModes } from "../../../api/queries";
import type { ModeResponse } from "../../../generated/ModeResponse";
import { modesOf } from "../../bots/facts";
import { isEditable, type ModeValues, newValues, valuesOf } from "../modeForm";
import { LOADING_MODES, MODE_GONE, modesFailed, NO_LONGER_EDITABLE } from "../texts";
import { LockedMode } from "./LockedMode";
import { ModeEditor } from "./ModeEditor";
import { BackLink } from "./ModeParts";

/** A new mode's form. */
export function NewModeScreen() {
  return <ModeEditor initial={newValues()} />;
}

/** What the locked view shows, so a change of any of it shows at once. */
function lockedKey(mode: ModeResponse): string {
  return JSON.stringify([mode.name, mode.visibility, mode.steps]);
}

/** Mode `id`: loading, failed, gone, or its form or locked view. */
export function ModeScreen({ id }: { id: string }) {
  const query = useModes();
  const known = modesOf(query);
  // The row an open form last showed: what it stays on while no row is read. Dropping the form
  // drops this, which holds arguments the server has since cut (S83).
  const opened = useRef<ModeResponse | null>(null);
  // Whether a form was dropped, and the locked view says why.
  const dropped = useRef(false);
  const [copy, setCopy] = useState<ModeValues | null>(null);
  const read = typeof known === "string" ? undefined : known.find((mode) => mode.id === id);

  if (copy !== null) {
    return <ModeEditor key="copy" initial={copy} copying />;
  }
  if (read === undefined) {
    if (opened.current !== null) {
      return (
        <ModeEditor
          row={opened.current}
          initial={valuesOf(opened.current)}
          left={typeof known !== "string"}
        />
      );
    }
    if (known === "failed") {
      return (
        <main className="flex max-w-3xl flex-col gap-4 p-6">
          <p role="alert" className="text-sm text-destructive">
            {modesFailed(describeError(toApiError(query.error).error))}
          </p>
          <BackLink />
        </main>
      );
    }
    if (known === "loading") {
      return (
        <main className="p-6">
          <p className="text-sm text-muted-foreground">{LOADING_MODES}</p>
        </main>
      );
    }
    return (
      <main className="flex max-w-3xl flex-col gap-4 p-6">
        <p role="alert">{MODE_GONE}</p>
        <BackLink />
      </main>
    );
  }
  if (isEditable(read)) {
    opened.current = read;
    dropped.current = false;
    return <ModeEditor row={read} initial={valuesOf(read)} left={false} onCopy={setCopy} />;
  }
  if (opened.current !== null) {
    opened.current = null;
    dropped.current = true;
  }
  return (
    <LockedMode
      key={lockedKey(read)}
      mode={read}
      note={dropped.current ? NO_LONGER_EDITABLE : undefined}
      onCopy={setCopy}
    />
  );
}
