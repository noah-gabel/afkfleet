// One section of a mode (Plan.md P11.9; ADR-0020, U68, R158): its heading and hint, its steps in
// their order, and its own Add step. A step moves only within its section: each section keeps
// its own list, so nothing can carry a step to another one. The server judges every rule but
// the most steps a form offers.
import { useId } from "react";
import { type UseFormReturn, useFieldArray } from "react-hook-form";

import { Button } from "../../../components/ui/button";
import { type ModeValues, newStep, type Section } from "../modeForm";
import { ADD_STEP, AT_MOST_STEPS, SECTION_TEXT, stepMoved } from "../texts";
import { UnknownSteps } from "./ModeParts";
import { StepCard, type StepView } from "./StepCard";

/** `section` of `form`; `unknown` steps show as a line each, `full` when no more can be added. */
export function StepSection({
  form,
  section,
  view,
  full,
  unknown,
  announce,
}: {
  form: UseFormReturn<ModeValues>;
  section: Section;
  view: StepView;
  full: boolean;
  unknown: number;
  announce: (text: string) => void;
}) {
  const hintId = useId();
  const steps = useFieldArray({ control: form.control, name: section });
  const text = SECTION_TEXT[section];

  function move(from: number, to: number): void {
    steps.move(from, to);
    announce(stepMoved(from + 1, to + 1, steps.fields.length));
  }

  return (
    <fieldset className="flex flex-col gap-4" aria-describedby={hintId}>
      <legend className="mb-2 text-lg font-semibold">{text.heading}</legend>
      <p id={hintId} className="text-sm text-muted-foreground">
        {text.hint}
      </p>
      {steps.fields.map((field, index) => (
        <StepCard
          key={field.id}
          form={form}
          section={section}
          index={index}
          count={steps.fields.length}
          view={view}
          onMove={move}
          onRemove={steps.remove}
        />
      ))}
      <UnknownSteps count={unknown} />
      {view.readOnly ? null : (
        <div className="flex flex-wrap items-center gap-3">
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={view.locked || full}
            onClick={() => steps.append(newStep())}
          >
            {ADD_STEP}
          </Button>
          {full ? <span className="text-sm text-muted-foreground">{AT_MOST_STEPS}</span> : null}
        </div>
      )}
    </fieldset>
  );
}
