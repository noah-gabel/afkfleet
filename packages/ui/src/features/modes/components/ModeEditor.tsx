// A mode's form (Plan.md P11.9; ADR-0020, U62, U67, U68, S12, S15, S16, S18, S82, S84, R5,
// R157–R164): new, or filled from a row the viewer may edit.
//
// - **Every request is built from the form,** once at submit, never by echoing an answer, at the
//   `version` the form was filled from, never the list's newest. It's a direct call: a chat step
//   can hold a password (R157).
// - **Save only when something changed:** while an existing mode's form is what was loaded, nothing
//   is sent, so an untouched save can't move a version or restart a bot (S84).
// - **A command off the allowlist:** on `mode_needs_step_up` the same request goes to the
//   `/with-commands` twin through `useStepUpAction`; the form stays locked, so what's sent is what
//   was shown (S16, R162).
// - **The server stays the authority:** its 422 shows at the field it names, through the order
//   sent (R158); `validated()` refuses only what the wire can't carry (S82). The form's "resolver"
//   checks nothing: it makes every submit start clean, so an error the server put on a field never
//   blocks the next save.
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useId, useRef, useState } from "react";
import { type Path, type Resolver, useForm, useWatch } from "react-hook-form";
import { toast } from "sonner";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { MODE_MAX_STEPS } from "../../../api/limits";
import { FormField } from "../../../components/FormField";
import { RetryWaitMessage } from "../../../components/RetryWaitMessage";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import type { CommandError } from "../../../generated/CommandError";
import type { CreateModeRequest } from "../../../generated/CreateModeRequest";
import type { ModeResponse } from "../../../generated/ModeResponse";
import { useRetryWait } from "../../../hooks/useRetryWait";
import { useAlive } from "../../../hooks/useStepGuard";
import { fieldErrorsOf, showFieldErrors } from "../../../lib/formErrors";
import { isStepUpRequired } from "../../../lib/stepUp";
import { StepUpPrompt } from "../../admin/components/StepUpPrompt";
import { useStepUpAction } from "../../admin/hooks/useStepUpAction";
import { rememberMode, rereadModes } from "../cache";
import { type ModeValues, SECTIONS, stepsOf, stepTotal, valuesOf } from "../modeForm";
import {
  CANCEL,
  CAP_REACHED,
  created,
  NAME_FIELD,
  NEW_MODE,
  NOTHING_TO_SAVE,
  SAVE,
  SAVE_STEP_UP,
  SAVED,
  STARTS_PRIVATE,
  VISIBILITY_FIELD,
  VISIBILITY_PRIVATE,
  VISIBILITY_SHARED,
} from "../texts";
import { BackLink, ModeSummary } from "./ModeParts";
import { StepSection } from "./StepSection";

/** The one write's key. */
const SAVE_KEY = "save";

/** See the module comment. */
const startClean: Resolver<ModeValues> = async (values) => ({ values, errors: {} });

/** The form of `row`, or of a new mode without one, starting from `initial`. */
export function ModeEditor({ row, initial }: { row?: ModeResponse; initial: ModeValues }) {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const alive = useAlive();
  const wait = useRetryWait();
  const formId = useId();
  // The version the form was filled from: what an edit sends, never the list's newest.
  const [version, setVersion] = useState(row?.version ?? 1);
  const [sending, setSending] = useState(false);
  const [formError, setFormError] = useState<string>();
  const [announcement, setAnnouncement] = useState("");
  const writes = useStepUpAction();
  const form = useForm<ModeValues>({ resolver: startClean, defaultValues: initial });
  const paths = useRef<Readonly<Record<string, Path<ModeValues>>>>({});
  const shownSteps = useWatch({ control: form.control, name: [...SECTIONS] });
  const total = stepTotal({
    atStart: shownSteps[0],
    repeating: shownSteps[1],
    onRespawn: shownSteps[2],
  });

  /** What a refusal other than the step-up's shows: a wait, a field's text, or a line. */
  function show(error: CommandError): void {
    if (wait.start(error)) {
      return;
    }
    const details = error.api;
    if (details?.status === 409 && row === undefined) {
      setFormError(CAP_REACHED);
    } else if (details?.status === 403) {
      setFormError(details.message);
      rereadModes(queryClient);
    } else if (!showFieldErrors(form.setError, error, paths.current)) {
      const fields = fieldErrorsOf(error);
      setFormError(
        fields.length > 0 ? fields.map((field) => field.message).join("\n") : describeError(error),
      );
    }
  }

  /** What a save's answer changes: the list, a toast, and the screen. */
  function answered(saved: ModeResponse): void {
    rememberMode(queryClient, saved);
    if (!alive.current) {
      return;
    }
    if (row === undefined) {
      toast.success(created(saved.name));
      void navigate({ to: "/modes/$id", params: { id: saved.id }, replace: true });
    } else {
      toast.success(saved.version === version ? NOTHING_TO_SAVE : SAVED);
      setVersion(saved.version);
      form.reset(valuesOf(saved));
    }
  }

  /** The twin, after the step-up it asks for (S16). */
  function throughTwin(send: () => Promise<ModeResponse>): void {
    void writes.attempt({
      key: SAVE_KEY,
      run: async () => {
        try {
          answered(await send());
        } catch (rejection) {
          const error = toApiError(rejection).error;
          if (isStepUpRequired(error)) {
            throw rejection;
          }
          if (alive.current) {
            show(error);
          }
        }
      },
    });
  }

  /** `plain`, then `twin` when the server points there. */
  async function save(
    plain: () => Promise<ModeResponse>,
    twin: () => Promise<ModeResponse>,
  ): Promise<void> {
    setSending(true);
    try {
      answered(await plain());
    } catch (rejection) {
      const error = toApiError(rejection).error;
      if (error.api?.code === "mode_needs_step_up") {
        throughTwin(twin);
      } else if (alive.current) {
        show(error);
      }
    } finally {
      if (alive.current) {
        setSending(false);
      }
    }
  }

  const submit = form.handleSubmit((values) => {
    if (wait.waiting !== null || (row !== undefined && !form.formState.isDirty)) {
      return;
    }
    setFormError(undefined);
    wait.reset();
    const built = stepsOf(values);
    paths.current = built.paths;
    const body: CreateModeRequest = {
      name: values.name,
      visibility: values.visibility,
      steps: built.steps,
    };
    if (row === undefined) {
      void save(
        () => api.createMode(body),
        () => api.createModeWithCommands(body),
      );
    } else {
      const request = { version, ...body };
      void save(
        () => api.updateMode(row.id, request),
        () => api.updateModeWithCommands(row.id, request),
      );
    }
  });

  // While a write runs or the password is asked, what's sent is what's on the screen.
  const locked = sending || writes.prompt !== null || writes.running !== null;
  const view = { readOnly: false, locked, copying: false };
  const errors = form.formState.errors;
  const canSave = row === undefined || form.formState.isDirty;
  return (
    <main className="flex max-w-3xl flex-col gap-6 p-6">
      <h1 className="text-2xl font-semibold tracking-tight">{row?.name ?? NEW_MODE}</h1>
      {row === undefined ? null : <ModeSummary mode={row} />}
      <form className="flex flex-col gap-6" noValidate onSubmit={(event) => void submit(event)}>
        <FormField id={`${formId}-name`} label={NAME_FIELD} error={errors.name?.message}>
          {(control) => (
            <Input {...control} autoComplete="off" readOnly={locked} {...form.register("name")} />
          )}
        </FormField>
        {row === undefined ? (
          <p className="text-sm text-muted-foreground">{STARTS_PRIVATE}</p>
        ) : (
          <FormField
            id={`${formId}-visibility`}
            label={VISIBILITY_FIELD}
            error={errors.visibility?.message}
          >
            {(control) =>
              row.visibility_editable ? (
                <select
                  {...control}
                  className="h-9 rounded-md border border-input bg-transparent px-3 text-sm"
                  disabled={locked}
                  {...form.register("visibility")}
                >
                  <option value="private">{VISIBILITY_PRIVATE}</option>
                  <option value="shared">{VISIBILITY_SHARED}</option>
                </select>
              ) : (
                <Input
                  {...control}
                  readOnly
                  value={row.visibility === "shared" ? VISIBILITY_SHARED : VISIBILITY_PRIVATE}
                />
              )
            }
          </FormField>
        )}
        {SECTIONS.map((section) => (
          <StepSection
            key={section}
            form={form}
            section={section}
            view={view}
            full={total >= MODE_MAX_STEPS}
            unknown={0}
            announce={setAnnouncement}
          />
        ))}
        <p role="status" className="sr-only">
          {announcement}
        </p>
        {formError === undefined ? null : (
          <p role="alert" className="text-sm whitespace-pre-line text-destructive">
            {formError}
          </p>
        )}
        <RetryWaitMessage wait={wait} />
        <div className="flex gap-2">
          <Button type="submit" disabled={locked || wait.waiting !== null || !canSave}>
            {SAVE}
          </Button>
          <Button
            type="button"
            variant="outline"
            disabled={writes.running !== null}
            onClick={() => {
              writes.cancel();
              void navigate({ to: "/modes" });
            }}
          >
            {CANCEL}
          </Button>
        </div>
      </form>
      {/* Outside the form: "Confirm it's you" is a form of its own. */}
      {writes.prompt === SAVE_KEY ? (
        <StepUpPrompt
          id="step-up-save"
          text={SAVE_STEP_UP}
          pending={writes.running !== null}
          onConfirm={writes.confirm}
          onCancel={writes.cancel}
        />
      ) : null}
      {writes.failure?.key === SAVE_KEY ? (
        <p role="alert" className="text-sm text-destructive">
          {writes.failure.message}
        </p>
      ) : null}
      <BackLink />
    </main>
  );
}
