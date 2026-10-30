// One mode's screen, and a new one's (Plan.md P11.9; ADR-0020, S14, S83, R160): the row comes from
// the every-page list, there being no single-mode route. An editable row gets the form, any other
// the locked view.
import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { useModes } from "../../../api/queries";
import type { ModeResponse } from "../../../generated/ModeResponse";
import { modesOf } from "../../bots/facts";
import { isEditable, newValues, valuesOf } from "../modeForm";
import { LOADING_MODES, MODE_GONE, modesFailed } from "../texts";
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
  const row = known.find((mode) => mode.id === id);
  if (row === undefined) {
    return (
      <main className="flex max-w-3xl flex-col gap-4 p-6">
        <p role="alert">{MODE_GONE}</p>
        <BackLink />
      </main>
    );
  }
  return isEditable(row) ? (
    <ModeEditor row={row} initial={valuesOf(row)} />
  ) : (
    <LockedMode key={lockedKey(row)} mode={row} />
  );
}
