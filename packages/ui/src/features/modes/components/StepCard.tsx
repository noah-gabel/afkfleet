// One step of a mode (Plan.md P11.9; ADR-0020, U68, U70, U72, U73, S15, R159, R170): what it does
// with only that kind's fields, a repeating step's wait, and its chance. Read-only, each choice
// shows as text in a read-only field, since a native select has no such state, and there's
// nothing to press. While a write runs or the password is asked, everything's locked for the
// moment, so what's sent is what's on the screen.
import { useId } from "react";
import {
  get,
  type Path,
  type UseFormRegisterReturn,
  type UseFormReturn,
  useWatch,
} from "react-hook-form";

import { FormField } from "../../../components/FormField";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import {
  ACTION_KINDS,
  type ModeValues,
  type Section,
  SLOT_KEYS,
  type StepValues,
  TIME_UNITS,
} from "../modeForm";
import {
  ACTION_TEXT,
  CHANCE_FIELD,
  EVERY_FIELD,
  HOLD_FIELD,
  HOLD_START,
  HOLD_STOP,
  JITTER_FIELD,
  MARK_CUT,
  MARK_CUT_IN_COPY,
  MAX_PITCH_FIELD,
  MAX_YAW_FIELD,
  MESSAGE_FIELD,
  MOVE_DOWN,
  MOVE_UP,
  PITCH_FIELD,
  REMOVE,
  SLOT_FIELD,
  SNEAK_FIELD,
  SNEAK_START,
  SNEAK_STOP,
  stepLabel,
  UNIT_TEXT,
  unitLabel,
  WHAT_IT_DOES,
  YAW_FIELD,
} from "../texts";

/** How a form shows: to edit, to edit but locked for the moment, or read-only. */
export interface StepView {
  /** The locked view: read-only fields and nothing to press (U73). */
  readonly readOnly: boolean;
  /** A write runs or the password is asked: nothing changes until it ends. */
  readonly locked: boolean;
  /** This is a copy: a cut command is the user's to complete (U63). */
  readonly copying: boolean;
}

/** A native select, as the bot's mode drop-down is. */
const SELECT = "h-9 rounded-md border border-input bg-transparent px-3 text-sm";

/** The choices of `on` for a kind that starts and stops. */
function onChoices(starts: string, stops: string) {
  return [
    { value: "start", text: starts },
    { value: "stop", text: stops },
  ];
}

/** Step `index` of `section`'s card. */
export function StepCard({
  form,
  section,
  index,
  count,
  view,
  onMove,
  onRemove,
  handle,
}: {
  form: UseFormReturn<ModeValues>;
  section: Section;
  index: number;
  count: number;
  view: StepView;
  onMove: (from: number, to: number) => void;
  onRemove: (index: number) => void;
  handle?: React.ReactNode;
}) {
  const baseId = useId();
  const legendId = `${baseId}-legend`;
  const step = useWatch({ control: form.control, name: `${section}.${index}` }) as
    | StepValues
    | undefined;
  if (step === undefined) {
    return null;
  }

  /** The form's path of this step's `field`. */
  const pathOf = (field: keyof StepValues) => `${section}.${index}.${field}` as Path<ModeValues>;
  /** What the form says is wrong with `field`. */
  const errorOf = (field: keyof StepValues): string | undefined =>
    (get(form.formState.errors, pathOf(field)) as { message?: string } | undefined)?.message;
  const number = (field: keyof StepValues, label: string) => (
    <FormField id={`${baseId}-${field}`} label={label} error={errorOf(field)}>
      {(control) => (
        <Input
          {...control}
          type="number"
          step="any"
          autoComplete="off"
          readOnly={view.readOnly || view.locked}
          {...form.register(pathOf(field))}
        />
      )}
    </FormField>
  );
  const choice = (
    field: keyof StepValues,
    label: string,
    options: readonly { readonly value: string; readonly text: string }[],
  ) => (
    <FormField id={`${baseId}-${field}`} label={label} error={errorOf(field)}>
      {(control) =>
        view.readOnly ? (
          <Input
            {...control}
            readOnly
            value={options.find((option) => option.value === step[field])?.text ?? ""}
          />
        ) : (
          <select
            {...control}
            className={SELECT}
            disabled={view.locked}
            {...form.register(pathOf(field))}
          >
            {options.map((option) => (
              <option key={option.value} value={option.value}>
                {option.text}
              </option>
            ))}
          </select>
        )
      }
    </FormField>
  );
  const time = (field: "every" | "jitter", unit: "everyUnit" | "jitterUnit", label: string) => (
    <FormField id={`${baseId}-${field}`} label={label} error={errorOf(field)}>
      {(control) => (
        <div className="flex gap-2">
          <Input
            {...control}
            type="number"
            step="any"
            autoComplete="off"
            className="min-w-0 flex-1"
            readOnly={view.readOnly || view.locked}
            {...form.register(pathOf(field))}
          />
          <UnitField
            label={unitLabel(label)}
            value={step[unit]}
            view={view}
            register={form.register(pathOf(unit))}
          />
        </div>
      )}
    </FormField>
  );
  const cut = step.cutCommand !== null && step.message.trim() === step.cutCommand;

  return (
    <fieldset className="flex flex-col gap-3 rounded-md border border-input p-4">
      <legend id={legendId} className="flex items-center gap-2 px-1 text-sm font-medium">
        {handle}
        {stepLabel(index + 1)}
      </legend>
      {choice(
        "kind",
        WHAT_IT_DOES,
        ACTION_KINDS.map((kind) => ({ value: kind, text: ACTION_TEXT[kind] })),
      )}
      {step.kind === "look" ? (
        <>
          {number("yaw", YAW_FIELD)}
          {number("pitch", PITCH_FIELD)}
        </>
      ) : null}
      {step.kind === "rotate_random" ? (
        <>
          {number("maxYaw", MAX_YAW_FIELD)}
          {number("maxPitch", MAX_PITCH_FIELD)}
        </>
      ) : null}
      {step.kind === "sneak" ? choice("on", SNEAK_FIELD, onChoices(SNEAK_START, SNEAK_STOP)) : null}
      {step.kind === "hold_use" ? choice("on", HOLD_FIELD, onChoices(HOLD_START, HOLD_STOP)) : null}
      {step.kind === "select_hotbar_slot"
        ? choice(
            "slot",
            SLOT_FIELD,
            SLOT_KEYS.map((key) => ({ value: key, text: key })),
          )
        : null}
      {step.kind === "send_chat" ? (
        <>
          <FormField id={`${baseId}-message`} label={MESSAGE_FIELD} error={errorOf("message")}>
            {(control) => (
              <Input
                {...control}
                autoComplete="off"
                readOnly={view.readOnly || view.locked}
                {...form.register(pathOf("message"))}
              />
            )}
          </FormField>
          {cut ? (
            <p className="text-sm text-muted-foreground">
              {view.copying ? MARK_CUT_IN_COPY : MARK_CUT}
            </p>
          ) : null}
        </>
      ) : null}
      {section === "repeating" ? (
        <>
          {time("every", "everyUnit", EVERY_FIELD)}
          {time("jitter", "jitterUnit", JITTER_FIELD)}
        </>
      ) : null}
      {number("chance", CHANCE_FIELD)}
      {view.readOnly ? null : (
        <div className="flex flex-wrap gap-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={view.locked || index === 0}
            aria-describedby={legendId}
            onClick={() => onMove(index, index - 1)}
          >
            {MOVE_UP}
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={view.locked || index === count - 1}
            aria-describedby={legendId}
            onClick={() => onMove(index, index + 1)}
          >
            {MOVE_DOWN}
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={view.locked}
            aria-describedby={legendId}
            onClick={() => onRemove(index)}
          >
            {REMOVE}
          </Button>
        </div>
      )}
    </fieldset>
  );
}

/** A time field's unit: a drop-down, or its name read-only. */
function UnitField({
  label,
  value,
  view,
  register,
}: {
  label: string;
  value: StepValues["everyUnit"];
  view: StepView;
  register: UseFormRegisterReturn;
}) {
  if (view.readOnly) {
    return <Input aria-label={label} readOnly className="w-28" value={UNIT_TEXT[value]} />;
  }
  return (
    <select aria-label={label} className={SELECT} disabled={view.locked} {...register}>
      {TIME_UNITS.map((unit) => (
        <option key={unit} value={unit}>
          {UNIT_TEXT[unit]}
        </option>
      ))}
    </select>
  );
}
