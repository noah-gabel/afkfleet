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
// - **The 409s:** `mode_changed` shows the server's text with "Load again", which reads the list
//   again and fills the form from it: only a read that succeeded does, and a failed one keeps the
//   notice and the edits and says why (U64). `mode_in_use_by_others` shows the server's text with
//   the app's explanation, and the form stays editable (U65). Nothing but "Load again" and a save
//   resets the form, so a refetch never loses edits.
// - **Copies:** "Make a copy" hands the form's values to `ModeScreen`, which opens them as a new
//   mode. "Save as copy", in the two notices and on a row that left the list, sends the form as a
//   new private mode; a name still the loaded one, which would always be refused, becomes the
//   suggested one on the form first, so what's sent is what's on the screen (U63, U67, R168).
// - **The server stays the authority:** its 422 shows at the field it names, through the order
//   sent (R158); `validated()` refuses only what the wire can't carry (S82). The form's "resolver"
//   checks nothing: it makes every submit start clean, so an error the server put on a field never
//   blocks the next save.
import { type QueryClient, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useId, useRef, useState } from "react";
import { type Path, type Resolver, useForm, useWatch } from "react-hook-form";
import { toast } from "sonner";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { MODE_MAX_STEPS } from "../../../api/limits";
import { MODES_LIST_KEY } from "../../../api/queries";
import { FormField } from "../../../components/FormField";
import { RetryWaitMessage } from "../../../components/RetryWaitMessage";
import { Alert, AlertDescription } from "../../../components/ui/alert";
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
import {
  copyOfValues,
  isEditable,
  type ModeValues,
  SECTIONS,
  stepsOf,
  stepTotal,
  valuesOf,
} from "../modeForm";
import {
  CANCEL,
  CAP_REACHED,
  copyName,
  created,
  IN_USE_BY_OTHERS,
  LEFT_THE_LIST,
  LOAD_AGAIN,
  MAKE_A_COPY,
  modesFailed,
  NAME_FIELD,
  NEW_MODE,
  NOTHING_TO_SAVE,
  SAVE,
  SAVE_AS_COPY,
  SAVE_STEP_UP,
  SAVED,
  STARTS_PRIVATE,
  savedCopy,
  VISIBILITY_FIELD,
  VISIBILITY_PRIVATE,
  VISIBILITY_SHARED,
} from "../texts";
import { DeleteMode } from "./DeleteMode";
import { BackLink, ModeSummary } from "./ModeParts";
import { StepSection } from "./StepSection";

/** The one write's key. */
const SAVE_KEY = "save";

/** See the module comment. */
const startClean: Resolver<ModeValues> = async (values) => ({ values, errors: {} });

/** A 409 the form shows above its Save: the server's text, and what it offers. */
interface Conflict {
  readonly kind: "changed" | "others";
  readonly text: string;
}

/** The modes the list's cache holds. */
function cachedModes(queryClient: QueryClient): readonly ModeResponse[] {
  return (queryClient.getQueryData(MODES_LIST_KEY)?.pages ?? []).flatMap((page) => page.items);
}

/**
 * The form of `row`, or of a new mode without one, starting from `initial`. `left`: the row left
 * the list, so the form stays but nothing here can be saved over it (S83). `copying`: the new
 * mode is a copy, a cut command the user's to complete. `onCopy` opens a copy of the form.
 */
export function ModeEditor({
  row,
  initial,
  left = false,
  copying = false,
  onCopy,
}: {
  row?: ModeResponse;
  initial: ModeValues;
  left?: boolean;
  copying?: boolean;
  onCopy?: (values: ModeValues) => void;
}) {
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
  const [conflict, setConflict] = useState<Conflict | null>(null);
  const [reloadFailed, setReloadFailed] = useState<string | null>(null);
  const [goneToSave, setGoneToSave] = useState(false);
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
  function show(error: CommandError, creating: boolean): void {
    if (wait.start(error)) {
      return;
    }
    const details = error.api;
    if (details?.status === 409 && details.code === "mode_changed") {
      setConflict({ kind: "changed", text: details.message });
    } else if (details?.status === 409 && details.code === "mode_in_use_by_others") {
      setConflict({ kind: "others", text: details.message });
    } else if (details?.status === 409 && creating) {
      setFormError(CAP_REACHED);
    } else if (details?.status === 404 && !creating) {
      setGoneToSave(true);
      rereadModes(queryClient);
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
  function answered(saved: ModeResponse, creating: boolean): void {
    rememberMode(queryClient, saved);
    if (!alive.current) {
      return;
    }
    if (creating) {
      toast.success(copying || row !== undefined ? savedCopy(saved.name) : created(saved.name));
      void navigate({ to: "/modes/$id", params: { id: saved.id }, replace: true });
    } else {
      toast.success(saved.version === version ? NOTHING_TO_SAVE : SAVED);
      setVersion(saved.version);
      form.reset(valuesOf(saved));
    }
  }

  /** The twin, after the step-up it asks for (S16). */
  function throughTwin(send: () => Promise<ModeResponse>, creating: boolean): void {
    void writes.attempt({
      key: SAVE_KEY,
      run: async () => {
        try {
          answered(await send(), creating);
        } catch (rejection) {
          const error = toApiError(rejection).error;
          if (isStepUpRequired(error)) {
            throw rejection;
          }
          if (alive.current) {
            show(error, creating);
          }
        }
      },
    });
  }

  /** `plain`, then `twin` when the server points there. */
  async function save(
    plain: () => Promise<ModeResponse>,
    twin: () => Promise<ModeResponse>,
    creating: boolean,
  ): Promise<void> {
    setSending(true);
    try {
      answered(await plain(), creating);
    } catch (rejection) {
      const error = toApiError(rejection).error;
      if (error.api?.code === "mode_needs_step_up") {
        throughTwin(twin, creating);
      } else if (alive.current) {
        show(error, creating);
      }
    } finally {
      if (alive.current) {
        setSending(false);
      }
    }
  }

  const submit = form.handleSubmit((values) => {
    if (
      wait.waiting !== null ||
      left ||
      goneToSave ||
      (row !== undefined && !form.formState.isDirty)
    ) {
      return;
    }
    setFormError(undefined);
    setConflict(null);
    setReloadFailed(null);
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
        true,
      );
    } else {
      const request = { version, ...body };
      void save(
        () => api.updateMode(row.id, request),
        () => api.updateModeWithCommands(row.id, request),
        false,
      );
    }
  });

  /** The form as a new private mode, under the suggested name if it still has the loaded one. */
  function saveAsCopy(): void {
    if (form.getValues("name").trim() === form.formState.defaultValues?.name) {
      form.setValue("name", copyName(form.getValues("name").trim()), { shouldDirty: true });
    }
    form.clearErrors();
    setFormError(undefined);
    setReloadFailed(null);
    wait.reset();
    const values = form.getValues();
    const built = stepsOf(values);
    paths.current = built.paths;
    const body: CreateModeRequest = {
      name: values.name,
      visibility: "private",
      steps: built.steps,
    };
    void save(
      () => api.createMode(body),
      () => api.createModeWithCommands(body),
      true,
    );
  }

  /**
   * Reads the list again after a 409 and fills the form from it. A read that fails changes nothing
   * but saying why: the notice and the edits stay. A row the form can't hold any more goes to the
   * locked view, by `ModeScreen` (S83).
   */
  async function loadAgain(): Promise<void> {
    if (row === undefined) {
      return;
    }
    setReloadFailed(null);
    try {
      await queryClient.refetchQueries(
        { queryKey: MODES_LIST_KEY, exact: true },
        { throwOnError: true },
      );
    } catch (rejection) {
      if (alive.current) {
        setReloadFailed(modesFailed(describeError(toApiError(rejection).error)));
      }
      return;
    }
    if (!alive.current) {
      return;
    }
    setConflict(null);
    const fresh = cachedModes(queryClient).find((one) => one.id === row.id);
    if (fresh !== undefined && isEditable(fresh)) {
      setVersion(fresh.version);
      form.reset(valuesOf(fresh));
    }
  }

  const gone = left || goneToSave;
  // While a write runs or the password is asked, what's sent is what's on the screen.
  const locked = sending || writes.prompt !== null || writes.running !== null;
  const view = { readOnly: false, locked, copying };
  const keepAsCopy = (
    <Button
      type="button"
      variant="outline"
      className="self-start"
      disabled={locked || wait.waiting !== null}
      onClick={saveAsCopy}
    >
      {SAVE_AS_COPY}
    </Button>
  );
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
        {conflict === null || gone ? null : (
          // One live region: the Alert's own.
          <Alert>
            <AlertDescription className="flex flex-col gap-2 text-foreground">
              <p>{conflict.text}</p>
              {conflict.kind === "others" ? <p>{IN_USE_BY_OTHERS}</p> : null}
              {reloadFailed === null ? null : <p className="text-destructive">{reloadFailed}</p>}
              {conflict.kind === "changed" ? (
                <Button
                  type="button"
                  variant="outline"
                  className="self-start"
                  onClick={() => void loadAgain()}
                >
                  {LOAD_AGAIN}
                </Button>
              ) : null}
              {keepAsCopy}
            </AlertDescription>
          </Alert>
        )}
        {gone ? (
          <div className="flex flex-col gap-2">
            <p role="alert">{LEFT_THE_LIST}</p>
            {keepAsCopy}
          </div>
        ) : null}
        <RetryWaitMessage wait={wait} />
        <div className="flex gap-2">
          {gone ? null : (
            <Button type="submit" disabled={locked || wait.waiting !== null || !canSave}>
              {SAVE}
            </Button>
          )}
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
      {onCopy === undefined || gone ? null : (
        <Button
          type="button"
          variant="outline"
          className="self-start"
          disabled={locked}
          onClick={() => onCopy(copyOfValues(form.getValues()))}
        >
          {MAKE_A_COPY}
        </Button>
      )}
      {row?.deletable && !gone ? (
        <DeleteMode mode={row} writes={writes} onGone={() => setGoneToSave(true)} />
      ) : null}
      <BackLink />
    </main>
  );
}
