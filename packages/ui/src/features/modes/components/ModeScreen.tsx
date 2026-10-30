// One mode's screen, and a new one's (Plan.md P11.9; ADR-0020, S14, S83, R160): the row comes from
// the every-page list, there being no single-mode route. An editable row gets the form, any other
// the locked view.
//
// While the form is open, the list can change under it (S83):
// - a newer version changes nothing but the flags it offers by: Save's 409 says so;
// - a row it can no longer hold, one whose commands may be cut for this viewer, drops to the locked
//   view, the edits gone, so what the server just cut isn't kept on the screen;
// - a row gone keeps the form, with its notice: a delete and a lost view look the same, and the
//   edits can still be saved as a copy. Only data actually read says a row is gone, never an
//   empty or reset query, so the form stays mounted through those.
import { useRef } from "react";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { useModes } from "../../../api/queries";
import type { ModeResponse } from "../../../generated/ModeResponse";
import { modesOf } from "../../bots/facts";
import { isEditable, newValues, valuesOf } from "../modeForm";
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
  // The row a form opened on, and last showed: what it stays on while no row is read.
  const opened = useRef<ModeResponse | null>(null);
  const read = typeof known === "string" ? undefined : known.find((mode) => mode.id === id);

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
    return <ModeEditor row={read} initial={valuesOf(read)} left={false} />;
  }
  const lost = opened.current !== null;
  return (
    <LockedMode key={lockedKey(read)} mode={read} note={lost ? NO_LONGER_EDITABLE : undefined} />
  );
}
