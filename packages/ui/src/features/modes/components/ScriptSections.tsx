// A scripted mode's sections (Plan.md P15.25, P15.35; ADR-0036, D8, D10, D11; ADR-0037, C3, C5;
// ADR-0042, G13; ADR-0045): the script in the editor, with the examples to start from on a new
// mode; the safety settings (the leash, the tool guard, the blocks a held left button mines); the
// declared messages with their typed values; the declared chat patterns with their captures, each
// that may count at any time marked as the riskier kind.
//
// Every field is the form's (scriptForm.ts) and shows the server's error at it; the script's own
// shows at its line in the editor. Read-only, every field keeps its text, and nothing can be added
// or removed.
import { type ReactNode, useId, useState } from "react";
import {
  Controller,
  type FieldPath,
  get,
  type UseFormReturn,
  useFieldArray,
  useWatch,
} from "react-hook-form";

import {
  SCRIPT_MAX_BLOCKS,
  SCRIPT_MAX_CAPTURES,
  SCRIPT_MAX_MESSAGES,
  SCRIPT_MAX_PATTERNS,
  SCRIPT_MAX_SIGNS,
  SCRIPT_MAX_VALUES,
} from "../../../api/limits";
import { FormField } from "../../../components/FormField";
import { NativeSelect } from "../../../components/NativeSelect";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import type { FieldError } from "../../../generated/FieldError";
import { EXAMPLES } from "../../scripts/api";
import { type Place, ScriptEditor } from "../../scripts/components/ScriptEditor";
import type { ModeValues } from "../modeForm";
import {
  CAPTURE_TYPES,
  declaredOf,
  newCapture,
  newMessage,
  newPattern,
  newSign,
  newValue,
  PATTERN_WHENS,
  scriptValuesOf,
  VALUE_TYPES,
} from "../scriptForm";
import {
  ADD_BLOCK,
  ADD_CAPTURE,
  ADD_MESSAGE,
  ADD_PATTERN,
  ADD_SIGN,
  ADD_VALUE,
  ANY_TIME_WARNING,
  atMost,
  BLOCKS_HEADING,
  BLOCKS_HINT,
  blockLabel,
  CAPTURE_TYPE_TEXT,
  CAPTURES_HEADING,
  captureLabel,
  DECIMAL_MARK_FIELD,
  DECIMAL_MARK_TEXT,
  KEY_FIELD,
  LEASH_FIELD,
  LEASH_HINT,
  LINE_FIELD,
  MAX_FIELD,
  MESSAGE_TEXT_FIELD,
  MESSAGES_HEADING,
  MESSAGES_HINT,
  MIN_FIELD,
  messageLabel,
  NAME_OF_CAPTURE,
  NAME_OF_VALUE,
  PATTERNS_HEADING,
  PATTERNS_HINT,
  PLACES_FIELD,
  patternLabel,
  removeBlock,
  removeCapture,
  removeMessage,
  removePattern,
  removeSign,
  removeValue,
  SAFETY_HEADING,
  SAFETY_HINT,
  SCRIPT_HEADING,
  SCRIPT_HINT,
  SIGNS_HEADING,
  SIGNS_HINT,
  signLabel,
  signLineField,
  TEMPLATE_FIELD,
  TEMPLATE_HINT,
  TEMPLATE_NONE,
  TEMPLATE_USE,
  TOOL_GUARD_FIELD,
  TOOL_GUARD_HINT,
  TYPE_FIELD,
  VALUE_TYPE_TEXT,
  VALUES_HEADING,
  valueLabel,
  WHEN_FIELD,
  WHEN_TEXT,
  WORDS_FIELD,
  WORDS_HINT,
} from "../texts";
import type { StepView } from "./StepCard";

/** The form and how it may be used, as every part below takes them. */
interface Part {
  readonly form: UseFormReturn<ModeValues>;
  readonly view: StepView;
}

/** Whether a field takes typing: not read-only, not locked by a write. */
function frozen(view: StepView): boolean {
  return view.readOnly || view.locked;
}

/** The error the form holds at `name`, if any. */
function errorAt(form: UseFormReturn<ModeValues>, name: string): string | undefined {
  const found: unknown = get(form.formState.errors, name);
  return typeof found === "object" && found !== null && "message" in found
    ? String(found.message)
    : undefined;
}

/** A text field of the form at `name`. */
function TextField({
  form,
  view,
  name,
  label,
  hint,
  mono = false,
}: Part & { name: FieldPath<ModeValues>; label: string; hint?: string; mono?: boolean }) {
  const id = useId();
  return (
    <FormField
      id={id}
      label={label}
      {...(hint === undefined ? {} : { hint })}
      error={errorAt(form, name)}
    >
      {(control) => (
        <Input
          {...control}
          autoComplete="off"
          spellCheck={false}
          className={mono ? "font-mono" : undefined}
          readOnly={frozen(view)}
          {...form.register(name)}
        />
      )}
    </FormField>
  );
}

/** A closed choice of the form at `name`: `choices` as `[value, text]`. */
function ChoiceField({
  form,
  view,
  name,
  label,
  choices,
}: Part & {
  name: FieldPath<ModeValues>;
  label: string;
  choices: readonly (readonly [string, string])[];
}) {
  const id = useId();
  const value = String(useWatch({ control: form.control, name }) ?? "");
  return (
    <FormField id={id} label={label} error={errorAt(form, name)}>
      {(control) =>
        view.readOnly ? (
          <Input
            {...control}
            readOnly
            value={choices.find(([one]) => one === value)?.[1] ?? value}
          />
        ) : (
          <NativeSelect {...control} disabled={view.locked} {...form.register(name)}>
            {choices.map(([one, text]) => (
              <option key={one} value={one}>
                {text}
              </option>
            ))}
          </NativeSelect>
        )
      }
    </FormField>
  );
}

/** A section's card: its heading and hint, then `children`. */
function Card({ heading, hint, children }: { heading: string; hint: string; children: ReactNode }) {
  const hintId = useId();
  return (
    <div className="rounded-lg border bg-card p-3">
      <fieldset className="flex min-w-0 flex-col gap-3" aria-describedby={hintId}>
        <legend className="mb-1 text-base font-semibold">{heading}</legend>
        <p id={hintId} className="text-sm text-muted-foreground">
          {hint}
        </p>
        {children}
      </fieldset>
    </div>
  );
}

/** An item of a list in its own box: its label, its fields, and its Remove. */
function Item({
  label,
  remove,
  view,
  onRemove,
  children,
}: {
  label: string;
  remove: string;
  view: StepView;
  onRemove: () => void;
  children: ReactNode;
}) {
  return (
    <fieldset className="flex min-w-0 flex-col gap-3 rounded-lg border p-3">
      <legend className="px-1 text-sm font-semibold">{label}</legend>
      {children}
      {view.readOnly ? null : (
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="self-start"
          disabled={view.locked}
          aria-label={remove}
          onClick={onRemove}
        >
          Remove
        </Button>
      )}
    </fieldset>
  );
}

/** An Add button, with what it says once the most are there. */
function Add({
  view,
  full,
  max,
  what,
  label,
  onAdd,
}: {
  view: StepView;
  full: boolean;
  max: number;
  what: string;
  label: string;
  onAdd: () => void;
}) {
  if (view.readOnly) {
    return null;
  }
  return (
    <div className="flex flex-wrap items-center gap-3">
      <Button
        type="button"
        variant="outline"
        size="sm"
        disabled={view.locked || full}
        onClick={onAdd}
      >
        {label}
      </Button>
      {full ? <span className="text-sm text-muted-foreground">{atMost(max, what)}</span> : null}
    </div>
  );
}

/** The script's card: the examples on a new mode, then the editor. */
function ScriptCard({
  form,
  view,
  templates,
  serverProblem,
  focusAt,
}: Part & { templates: boolean; serverProblem: FieldError | null; focusAt: Place | null }) {
  const pickId = useId();
  const [picked, setPicked] = useState("");
  const [messages, patterns, signs] = useWatch({
    control: form.control,
    name: ["script.messages", "script.patterns", "script.signs"],
  });
  const declared = declaredOf({ ...form.getValues("script"), messages, patterns, signs });

  /** Replaces the body with the picked example's, as an edit. */
  function applyExample(): void {
    const example = EXAMPLES.find((one) => one.id === picked);
    if (example !== undefined) {
      // A reset, not a setValue: the lists inside the body are field arrays, which only a reset
      // fills anew. Its defaults stay the loaded ones, so Save still compares with what was read.
      form.reset(
        { ...form.getValues(), script: scriptValuesOf(example.script) },
        { keepDefaultValues: true },
      );
    }
  }

  return (
    <Card heading={SCRIPT_HEADING} hint={SCRIPT_HINT}>
      {templates && !view.readOnly ? (
        <FormField id={pickId} label={TEMPLATE_FIELD} hint={TEMPLATE_HINT} error={undefined}>
          {(control) => (
            <div className="flex flex-wrap items-center gap-2">
              <NativeSelect
                {...control}
                disabled={view.locked}
                value={picked}
                onChange={(event) => setPicked(event.target.value)}
              >
                <option value="" disabled>
                  {TEMPLATE_NONE}
                </option>
                {EXAMPLES.map((example) => (
                  <option key={example.id} value={example.id}>
                    {example.title}
                  </option>
                ))}
              </NativeSelect>
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={view.locked || picked === ""}
                onClick={applyExample}
              >
                {TEMPLATE_USE}
              </Button>
            </div>
          )}
        </FormField>
      ) : null}
      <Controller
        control={form.control}
        name="script.source"
        render={({ field }) => (
          <ScriptEditor
            label={SCRIPT_HEADING}
            value={field.value}
            onChange={field.onChange}
            readOnly={frozen(view)}
            serverProblem={serverProblem}
            declared={declared}
            focusAt={focusAt}
          />
        )}
      />
    </Card>
  );
}

/** The safety settings: the leash, the tool guard, the blocks it mines. */
function SafetyCard({ form, view }: Part) {
  const blocks = useFieldArray({ control: form.control, name: "script.blocks" });
  const full = blocks.fields.length >= SCRIPT_MAX_BLOCKS;
  return (
    <Card heading={SAFETY_HEADING} hint={SAFETY_HINT}>
      <div className="grid gap-3 @min-[40rem]/content:grid-cols-2">
        <TextField
          form={form}
          view={view}
          name="script.leash"
          label={LEASH_FIELD}
          hint={LEASH_HINT}
        />
        <TextField
          form={form}
          view={view}
          name="script.toolGuard"
          label={TOOL_GUARD_FIELD}
          hint={TOOL_GUARD_HINT}
        />
      </div>
      <fieldset className="flex min-w-0 flex-col gap-2">
        <legend className="mb-1 text-sm font-semibold">{BLOCKS_HEADING}</legend>
        <p className="text-sm text-muted-foreground">{BLOCKS_HINT}</p>
        {blocks.fields.map((field, b) => (
          <div key={field.id} className="flex items-end gap-2">
            <div className="min-w-0 flex-1">
              <TextField
                form={form}
                view={view}
                name={`script.blocks.${b}.block`}
                label={blockLabel(b + 1)}
                mono
              />
            </div>
            {view.readOnly ? null : (
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={view.locked}
                aria-label={removeBlock(b + 1)}
                onClick={() => blocks.remove(b)}
              >
                Remove
              </Button>
            )}
          </div>
        ))}
        <Add
          view={view}
          full={full}
          max={SCRIPT_MAX_BLOCKS}
          what="blocks"
          label={ADD_BLOCK}
          onAdd={() => blocks.append({ block: "" })}
        />
      </fieldset>
    </Card>
  );
}

/** A declared value's fields, by its type. */
function ValueFields({
  form,
  view,
  path,
}: Part & {
  path: `script.messages.${number}.values.${number}` | `script.signs.${number}.values.${number}`;
}) {
  const type = useWatch({ control: form.control, name: `${path}.type` });
  return (
    <div className="grid gap-3 @min-[40rem]/content:grid-cols-2">
      <TextField form={form} view={view} name={`${path}.name`} label={NAME_OF_VALUE} mono />
      <ChoiceField
        form={form}
        view={view}
        name={`${path}.type`}
        label={TYPE_FIELD}
        choices={VALUE_TYPES.map((one) => [one, VALUE_TYPE_TEXT[one]] as const)}
      />
      {type === "word" ? (
        <TextField
          form={form}
          view={view}
          name={`${path}.words`}
          label={WORDS_FIELD}
          hint={WORDS_HINT}
          mono
        />
      ) : (
        <>
          <TextField form={form} view={view} name={`${path}.min`} label={MIN_FIELD} />
          <TextField form={form} view={view} name={`${path}.max`} label={MAX_FIELD} />
        </>
      )}
      {type === "decimal" ? (
        <TextField form={form} view={view} name={`${path}.places`} label={PLACES_FIELD} />
      ) : null}
    </div>
  );
}

/** A declared message: its key, its text, its values. */
function MessageItem({
  form,
  view,
  index,
  onRemove,
}: Part & { index: number; onRemove: () => void }) {
  const values = useFieldArray({ control: form.control, name: `script.messages.${index}.values` });
  const full = values.fields.length >= SCRIPT_MAX_VALUES;
  return (
    <Item
      label={messageLabel(index + 1)}
      remove={removeMessage(index + 1)}
      view={view}
      onRemove={onRemove}
    >
      <TextField
        form={form}
        view={view}
        name={`script.messages.${index}.key`}
        label={KEY_FIELD}
        mono
      />
      <TextField
        form={form}
        view={view}
        name={`script.messages.${index}.text`}
        label={MESSAGE_TEXT_FIELD}
        mono
      />
      <p className="text-sm font-semibold">{VALUES_HEADING}</p>
      {values.fields.map((field, v) => (
        <Item
          key={field.id}
          label={valueLabel(v + 1)}
          remove={removeValue(v + 1)}
          view={view}
          onRemove={() => values.remove(v)}
        >
          <ValueFields form={form} view={view} path={`script.messages.${index}.values.${v}`} />
        </Item>
      ))}
      <Add
        view={view}
        full={full}
        max={SCRIPT_MAX_VALUES}
        what="values"
        label={ADD_VALUE}
        onAdd={() => values.append(newValue())}
      />
    </Item>
  );
}

/** The declared messages. */
function MessagesCard({ form, view }: Part) {
  const messages = useFieldArray({ control: form.control, name: "script.messages" });
  const full = messages.fields.length >= SCRIPT_MAX_MESSAGES;
  return (
    <Card heading={MESSAGES_HEADING} hint={MESSAGES_HINT}>
      {messages.fields.map((field, m) => (
        <MessageItem
          key={field.id}
          form={form}
          view={view}
          index={m}
          onRemove={() => messages.remove(m)}
        />
      ))}
      <Add
        view={view}
        full={full}
        max={SCRIPT_MAX_MESSAGES}
        what="messages"
        label={ADD_MESSAGE}
        onAdd={() => messages.append(newMessage())}
      />
    </Card>
  );
}

/** A declared sign: its key, its four lines, its values. */
function SignItem({ form, view, index, onRemove }: Part & { index: number; onRemove: () => void }) {
  const values = useFieldArray({ control: form.control, name: `script.signs.${index}.values` });
  const full = values.fields.length >= SCRIPT_MAX_VALUES;
  return (
    <Item
      label={signLabel(index + 1)}
      remove={removeSign(index + 1)}
      view={view}
      onRemove={onRemove}
    >
      <TextField
        form={form}
        view={view}
        name={`script.signs.${index}.key`}
        label={KEY_FIELD}
        mono
      />
      <div className="grid gap-3 @min-[40rem]/content:grid-cols-2">
        {([0, 1, 2, 3] as const).map((line) => (
          <TextField
            key={line}
            form={form}
            view={view}
            name={`script.signs.${index}.lines.${line}`}
            label={signLineField(line + 1)}
            mono
          />
        ))}
      </div>
      <p className="text-sm font-semibold">{VALUES_HEADING}</p>
      {values.fields.map((field, v) => (
        <Item
          key={field.id}
          label={valueLabel(v + 1)}
          remove={removeValue(v + 1)}
          view={view}
          onRemove={() => values.remove(v)}
        >
          <ValueFields form={form} view={view} path={`script.signs.${index}.values.${v}`} />
        </Item>
      ))}
      <Add
        view={view}
        full={full}
        max={SCRIPT_MAX_VALUES}
        what="values"
        label={ADD_VALUE}
        onAdd={() => values.append(newValue())}
      />
    </Item>
  );
}

/** The declared signs (API level 2). */
function SignsCard({ form, view }: Part) {
  const signs = useFieldArray({ control: form.control, name: "script.signs" });
  const full = signs.fields.length >= SCRIPT_MAX_SIGNS;
  return (
    <Card heading={SIGNS_HEADING} hint={SIGNS_HINT}>
      {signs.fields.map((field, s) => (
        <SignItem
          key={field.id}
          form={form}
          view={view}
          index={s}
          onRemove={() => signs.remove(s)}
        />
      ))}
      <Add
        view={view}
        full={full}
        max={SCRIPT_MAX_SIGNS}
        what="signs"
        label={ADD_SIGN}
        onAdd={() => signs.append(newSign())}
      />
    </Card>
  );
}

/** A capture's fields, by its type. */
function CaptureFields({
  form,
  view,
  path,
}: Part & { path: `script.patterns.${number}.captures.${number}` }) {
  const type = useWatch({ control: form.control, name: `${path}.type` });
  return (
    <div className="grid gap-3 @min-[40rem]/content:grid-cols-2">
      <TextField form={form} view={view} name={`${path}.name`} label={NAME_OF_CAPTURE} mono />
      <ChoiceField
        form={form}
        view={view}
        name={`${path}.type`}
        label={TYPE_FIELD}
        choices={CAPTURE_TYPES.map((one) => [one, CAPTURE_TYPE_TEXT[one]] as const)}
      />
      {type === "word" ? (
        <TextField
          form={form}
          view={view}
          name={`${path}.words`}
          label={WORDS_FIELD}
          hint={WORDS_HINT}
          mono
        />
      ) : (
        <>
          <TextField form={form} view={view} name={`${path}.min`} label={MIN_FIELD} />
          <TextField form={form} view={view} name={`${path}.max`} label={MAX_FIELD} />
        </>
      )}
      {type === "amount" ? (
        <ChoiceField
          form={form}
          view={view}
          name={`${path}.decimalMark`}
          label={DECIMAL_MARK_FIELD}
          choices={([".", ","] as const).map((mark) => [mark, DECIMAL_MARK_TEXT[mark]] as const)}
        />
      ) : null}
    </div>
  );
}

/** A declared chat pattern: its key, when it counts, its line, its captures. */
function PatternItem({
  form,
  view,
  index,
  onRemove,
}: Part & { index: number; onRemove: () => void }) {
  const captures = useFieldArray({
    control: form.control,
    name: `script.patterns.${index}.captures`,
  });
  const when = useWatch({ control: form.control, name: `script.patterns.${index}.when` });
  const full = captures.fields.length >= SCRIPT_MAX_CAPTURES;
  return (
    <Item
      label={patternLabel(index + 1)}
      remove={removePattern(index + 1)}
      view={view}
      onRemove={onRemove}
    >
      <TextField
        form={form}
        view={view}
        name={`script.patterns.${index}.key`}
        label={KEY_FIELD}
        mono
      />
      <ChoiceField
        form={form}
        view={view}
        name={`script.patterns.${index}.when`}
        label={WHEN_FIELD}
        choices={PATTERN_WHENS.map((one) => [one, WHEN_TEXT[one]] as const)}
      />
      {when === "any_time" ? (
        <Alert>
          <AlertDescription className="text-foreground">{ANY_TIME_WARNING}</AlertDescription>
        </Alert>
      ) : null}
      <TextField
        form={form}
        view={view}
        name={`script.patterns.${index}.text`}
        label={LINE_FIELD}
        mono
      />
      <p className="text-sm font-semibold">{CAPTURES_HEADING}</p>
      {captures.fields.map((field, c) => (
        <Item
          key={field.id}
          label={captureLabel(c + 1)}
          remove={removeCapture(c + 1)}
          view={view}
          onRemove={() => captures.remove(c)}
        >
          <CaptureFields form={form} view={view} path={`script.patterns.${index}.captures.${c}`} />
        </Item>
      ))}
      <Add
        view={view}
        full={full}
        max={SCRIPT_MAX_CAPTURES}
        what="captures"
        label={ADD_CAPTURE}
        onAdd={() => captures.append(newCapture())}
      />
    </Item>
  );
}

/** The declared chat patterns. */
function PatternsCard({ form, view }: Part) {
  const patterns = useFieldArray({ control: form.control, name: "script.patterns" });
  const full = patterns.fields.length >= SCRIPT_MAX_PATTERNS;
  return (
    <Card heading={PATTERNS_HEADING} hint={PATTERNS_HINT}>
      {patterns.fields.map((field, p) => (
        <PatternItem
          key={field.id}
          form={form}
          view={view}
          index={p}
          onRemove={() => patterns.remove(p)}
        />
      ))}
      <Add
        view={view}
        full={full}
        max={SCRIPT_MAX_PATTERNS}
        what="patterns"
        label={ADD_PATTERN}
        onAdd={() => patterns.append(newPattern())}
      />
    </Card>
  );
}

/**
 * A scripted mode's sections; `templates` offers the examples (a new mode), `serverProblem` is the
 * server's answer at `script.source`, and `focusAt` a place to show in the script.
 */
export function ScriptSections({
  form,
  view,
  templates = false,
  serverProblem = null,
  focusAt = null,
}: Part & { templates?: boolean; serverProblem?: FieldError | null; focusAt?: Place | null }) {
  return (
    <>
      <ScriptCard
        form={form}
        view={view}
        templates={templates}
        serverProblem={serverProblem}
        focusAt={focusAt}
      />
      <SafetyCard form={form} view={view} />
      <MessagesCard form={form} view={view} />
      <SignsCard form={form} view={view} />
      <PatternsCard form={form} view={view} />
    </>
  );
}
