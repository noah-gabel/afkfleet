// A mode the viewer can't edit, or that this version can't hold (Plan.md P11.9; ADR-0020, U73,
// R160, R161): the editor's own sections, cards, labels and values, read-only rather than
// disabled, so they keep the normal text color and screen readers read them; nothing to press
// but what the server's flags offer. A step this version doesn't know is one line. Nothing can
// be lost here, so it follows the row: `ModeScreen` keys it by what it shows.
import { useId, useState } from "react";
import { useForm } from "react-hook-form";

import { FormField } from "../../../components/FormField";
import { Input } from "../../../components/ui/input";
import type { ModeResponse } from "../../../generated/ModeResponse";
import { useStepUpAction } from "../../admin/hooks/useStepUpAction";
import { isKnownMode, type ModeValues, SECTIONS, unknownSteps, valuesOf } from "../modeForm";
import {
  MODE_GONE,
  NAME_FIELD,
  OTHER_STEPS,
  UNKNOWN_MODE,
  VISIBILITY_FIELD,
  visibilityText,
} from "../texts";
import { DeleteMode } from "./DeleteMode";
import { BackLink, ModeSummary, UnknownSteps } from "./ModeParts";
import type { StepView } from "./StepCard";
import { StepSection } from "./StepSection";

/** The view of a form nothing can change. */
const READ_ONLY: StepView = { readOnly: true, locked: false, copying: false };

/** Nothing to announce: nothing moves. */
function silent(): void {}

/** `mode`, locked; `note` says why it's locked when that needs saying. */
export function LockedMode({ mode, note }: { mode: ModeResponse; note?: string | undefined }) {
  const formId = useId();
  const form = useForm<ModeValues>({ defaultValues: valuesOf(mode) });
  const writes = useStepUpAction();
  const [gone, setGone] = useState(false);
  const unknown = unknownSteps(mode);
  const elsewhere = unknown.filter((section) => section === null).length;
  return (
    <main className="flex max-w-3xl flex-col gap-6 p-6">
      <h1 className="text-2xl font-semibold tracking-tight">{mode.name}</h1>
      <ModeSummary mode={mode} />
      {isKnownMode(mode) ? null : <p className="text-sm">{UNKNOWN_MODE}</p>}
      {note === undefined ? null : <p role="alert">{note}</p>}
      <div className="flex flex-col gap-6">
        <FormField id={`${formId}-name`} label={NAME_FIELD} error={undefined}>
          {(control) => <Input {...control} readOnly value={mode.name} />}
        </FormField>
        <FormField id={`${formId}-visibility`} label={VISIBILITY_FIELD} error={undefined}>
          {(control) => <Input {...control} readOnly value={visibilityText(mode.visibility)} />}
        </FormField>
        {SECTIONS.map((section) => (
          <StepSection
            key={section}
            form={form}
            section={section}
            view={READ_ONLY}
            full={false}
            unknown={unknown.filter((one) => one === section).length}
            announce={silent}
          />
        ))}
        {elsewhere === 0 ? null : (
          <fieldset className="flex flex-col gap-2">
            <legend className="mb-2 text-lg font-semibold">{OTHER_STEPS}</legend>
            <UnknownSteps count={elsewhere} />
          </fieldset>
        )}
      </div>
      {gone ? <p role="alert">{MODE_GONE}</p> : null}
      {mode.deletable && !gone ? (
        <DeleteMode mode={mode} writes={writes} onGone={() => setGone(true)} />
      ) : null}
      <BackLink />
    </main>
  );
}
