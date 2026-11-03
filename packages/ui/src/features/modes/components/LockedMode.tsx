// A mode the viewer can't edit, or that this version can't hold (Plan.md P11.9; ADR-0020, U73,
// R160, R161): the editor's own sections, cards, labels and values, read-only rather than
// disabled, so they keep the normal text color and screen readers read them; nothing to press
// but what the server's flags offer. A step this version doesn't know is one line. Nothing can
// be lost here, so it follows the row: `ModeScreen` keys it by what it shows.
import { useId, useState } from "react";
import { useForm } from "react-hook-form";

import { EditorColumns } from "../../../components/EditorColumns";
import { FormField } from "../../../components/FormField";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import type { ModeResponse } from "../../../generated/ModeResponse";
import { useStepUpAction } from "../../admin/hooks/useStepUpAction";
import {
  copyValues,
  isKnownMode,
  type ModeValues,
  SECTIONS,
  unknownSteps,
  valuesOf,
} from "../modeForm";
import {
  MAKE_A_COPY,
  MODE_GONE,
  NAME_FIELD,
  OTHER_STEPS,
  UNKNOWN_MODE,
  VISIBILITY_FIELD,
  visibilityText,
} from "../texts";
import { DeleteMode } from "./DeleteMode";
import { ModeSummary, UnknownSteps } from "./ModeParts";
import type { StepView } from "./StepCard";
import { StepSection } from "./StepSection";

/** The view of a form nothing can change. */
const READ_ONLY: StepView = { readOnly: true, locked: false, copying: false };

/** Nothing to announce: nothing moves. */
function silent(): void {}

/**
 * `mode`, locked; `note` says why it's locked when that needs saying, and `onCopy` opens a copy
 * of it, if this version can hold it (U63).
 */
export function LockedMode({
  mode,
  note,
  onCopy,
}: {
  mode: ModeResponse;
  note?: string | undefined;
  onCopy: (values: ModeValues) => void;
}) {
  const formId = useId();
  const form = useForm<ModeValues>({ defaultValues: valuesOf(mode) });
  const writes = useStepUpAction();
  const [gone, setGone] = useState(false);
  const unknown = unknownSteps(mode);
  const elsewhere = unknown.filter((section) => section === null).length;
  return (
    <EditorColumns
      box={
        <>
          <h1 className="text-lg font-semibold tracking-tight break-words">{mode.name}</h1>
          <ModeSummary mode={mode} />
          {isKnownMode(mode) ? null : <p className="text-sm">{UNKNOWN_MODE}</p>}
          {note === undefined ? null : <p role="alert">{note}</p>}
          <FormField id={`${formId}-name`} label={NAME_FIELD} error={undefined}>
            {(control) => <Input {...control} readOnly value={mode.name} />}
          </FormField>
          <FormField id={`${formId}-visibility`} label={VISIBILITY_FIELD} error={undefined}>
            {(control) => <Input {...control} readOnly value={visibilityText(mode.visibility)} />}
          </FormField>
        </>
      }
      sections={
        <>
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
            <div className="rounded-lg border bg-card p-3">
              <fieldset className="flex min-w-0 flex-col gap-2">
                <legend className="mb-2 text-base font-semibold">{OTHER_STEPS}</legend>
                <UnknownSteps count={elsewhere} />
              </fieldset>
            </div>
          )}
        </>
      }
      notes={gone ? <p role="alert">{MODE_GONE}</p> : null}
      apart={
        <>
          {isKnownMode(mode) ? (
            <Button type="button" variant="outline" onClick={() => onCopy(copyValues(mode))}>
              {MAKE_A_COPY}
            </Button>
          ) : null}
          {mode.deletable && !gone ? (
            <div className="w-full border-t pt-3">
              <DeleteMode mode={mode} writes={writes} onGone={() => setGone(true)} />
            </div>
          ) : null}
        </>
      }
    />
  );
}
