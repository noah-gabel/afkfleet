// A mode's form (Plan.md P11.9; ADR-0020, U62, U67, U68, S12, S15, S16, S18, S82, S84, R5,
// R157–R164): new, or filled from a row the viewer may edit.
//
// - **Every request is built from the form,** once at submit, never by echoing an answer, at the
//   `version` the form was filled from, never the list's newest. It's a direct call: a chat step
//   can hold a password (R157).
// - **Save only when something changed:** while an existing mode's form is what was loaded, nothing
//   is sent, so an untouched save can't move a version or restart a bot (S84).
// - **A command off the allowlist:** on `mode_needs_step_up` the same request goes to the
//   `/with-commands` twin within the same save, never asking for the password (ADR-0028); the
//   form stays locked, so what's sent is what was shown (S16, R162).
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
// - **Steps or a script** (P15.25, ADR-0036, D15): a new mode's kind is chosen here by an Admin or
//   the Owner, who alone write scripts; anyone else's new mode is steps. A scripted mode sends its
//   body and no steps; the server's error at `script.source` shows at its line in the editor, and
//   one in a one-line text (a message's, a pattern's) names its character.

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
import { EditorColumns } from "../../../components/EditorColumns";
import { FormField } from "../../../components/FormField";
import { NativeSelect } from "../../../components/NativeSelect";
import { RetryWaitMessage } from "../../../components/RetryWaitMessage";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import type { CommandError } from "../../../generated/CommandError";
import type { CreateModeRequest } from "../../../generated/CreateModeRequest";
import type { FieldError } from "../../../generated/FieldError";
import type { ModeResponse } from "../../../generated/ModeResponse";
import { useRetryWait } from "../../../hooks/useRetryWait";
import { useAlive } from "../../../hooks/useStepGuard";
import { fieldErrorsOf, showFieldErrors } from "../../../lib/formErrors";
import { inView } from "../../../lib/inView";
import { useWriteAction } from "../../admin/hooks/useWriteAction";
import type { Place } from "../../scripts/components/ScriptEditor";
import { rememberMode, rereadModes } from "../cache";
import {
  bodyOf,
  copyOfValues,
  isEditable,
  type ModeValues,
  SECTIONS,
  stepTotal,
  valuesOf,
} from "../modeForm";
import {
  atCharacter,
  CANCEL,
  CAP_REACHED,
  copyName,
  created,
  IN_USE_BY_OTHERS,
  KIND_FIELD,
  KIND_TEXT,
  LEFT_THE_LIST,
  LOAD_AGAIN,
  MAKE_A_COPY,
  modesFailed,
  NAME_FIELD,
  NEW_MODE,
  NOTHING_TO_SAVE,
  SAVE,
  SAVE_AS_COPY,
  SAVED,
  STARTS_PRIVATE,
  savedCopy,
  VISIBILITY_FIELD,
  VISIBILITY_PRIVATE,
  VISIBILITY_SHARED,
  visibilityText,
} from "../texts";
import { DeleteMode } from "./DeleteMode";
import { ModeSummary } from "./ModeParts";
import { ScriptSections } from "./ScriptSections";
import { StepSection } from "./StepSection";

/** See the module comment. */
const startClean: Resolver<ModeValues> = async (values) => ({ values, errors: {} });

/** A 409 the form shows above its Save: the server's text, and what it offers. */
interface Conflict {
  readonly kind: "changed" | "others";
  readonly text: string;
}

/**
 * `error`'s fields as the form shows them: the script's own, which the editor shows at its line,
 * apart; a one-line text's with the character it's at.
 */
function splitFields(error: CommandError): { script: FieldError | null; error: CommandError } {
  const fields = fieldErrorsOf(error);
  const script = fields.find((field) => field.path === "script.source") ?? null;
  const rest = fields
    .filter((field) => field.path !== "script.source")
    .map((field) =>
      field.path.endsWith(".text") && field.column !== undefined
        ? { ...field, message: atCharacter(field.message, field.column) }
        : field,
    );
  const shown: CommandError =
    error.api === undefined
      ? { ...error, fields: rest }
      : { ...error, api: { ...error.api, fields: rest } };
  return { script, error: shown };
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
  writesScripts = false,
  focusAt = null,
}: {
  row?: ModeResponse;
  initial: ModeValues;
  left?: boolean;
  copying?: boolean;
  onCopy?: (values: ModeValues) => void;
  /** Whether the viewer may write a scripted mode: an Admin or the Owner (ADR-0036, D15). */
  writesScripts?: boolean;
  /** A place in the script to show: a bot's script error links here (P15.26). */
  focusAt?: Place | null;
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
  const [reloading, setReloading] = useState(false);
  const [goneToSave, setGoneToSave] = useState(false);
  const [scriptProblem, setScriptProblem] = useState<FieldError | null>(null);
  const writes = useWriteAction();
  const form = useForm<ModeValues>({ resolver: startClean, defaultValues: initial });
  const paths = useRef<Readonly<Record<string, Path<ModeValues>>>>({});
  const shownSteps = useWatch({ control: form.control, name: [...SECTIONS] });
  const kind = useWatch({ control: form.control, name: "kind" });
  const total = stepTotal({
    atStart: shownSteps[0],
    repeating: shownSteps[1],
    onRespawn: shownSteps[2],
  });

  /** The request's common part from `values`, at `visibility`, and where its errors show. */
  function requestBody(
    values: ModeValues,
    visibility: ModeValues["visibility"],
  ): CreateModeRequest {
    const built = bodyOf(values);
    paths.current = built.paths;
    return {
      name: values.name,
      visibility,
      steps: built.steps,
      ...(built.script === undefined ? {} : { script: built.script }),
    };
  }

  /** What a refusal shows: a wait, a field's text, or a line. */
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
    } else {
      const split = splitFields(error);
      setScriptProblem(split.script);
      const fields = fieldErrorsOf(split.error);
      if (fields.length > 0 && !showFieldErrors(form.setError, split.error, paths.current)) {
        setFormError(fields.map((field) => field.message).join("\n"));
      } else if (fields.length === 0 && split.script === null) {
        setFormError(describeError(error));
      }
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

  /** `plain`, or `twin` when the server points there (S16). */
  async function send(
    plain: () => Promise<ModeResponse>,
    twin: () => Promise<ModeResponse>,
  ): Promise<ModeResponse> {
    try {
      return await plain();
    } catch (rejection) {
      if (toApiError(rejection).error.api?.code !== "mode_needs_step_up") {
        throw rejection;
      }
      return await twin();
    }
  }

  /** `plain`, then `twin` when the server points there. */
  async function save(
    plain: () => Promise<ModeResponse>,
    twin: () => Promise<ModeResponse>,
    creating: boolean,
  ): Promise<void> {
    setSending(true);
    try {
      answered(await send(plain, twin), creating);
    } catch (rejection) {
      if (alive.current) {
        show(toApiError(rejection).error, creating);
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
    setScriptProblem(null);
    wait.reset();
    const body = requestBody(values, values.visibility);
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
    setScriptProblem(null);
    wait.reset();
    const body = requestBody(form.getValues(), "private");
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
    setReloading(true);
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
    } finally {
      if (alive.current) {
        setReloading(false);
      }
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
  // While a write runs, what's sent is what's on the screen; and while Load again reads, nothing
  // starts that its answer would write over.
  const locked = sending || reloading || writes.running !== null;
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
    <EditorColumns
      form={{ id: formId, onSubmit: (event) => void submit(event) }}
      box={
        <>
          <h1 className="text-lg font-semibold tracking-tight break-words">
            {row?.name ?? NEW_MODE}
          </h1>
          {row === undefined ? null : <ModeSummary mode={row} />}
          <FormField id={`${formId}-name`} label={NAME_FIELD} error={errors.name?.message}>
            {(control) => (
              <Input {...control} autoComplete="off" readOnly={locked} {...form.register("name")} />
            )}
          </FormField>
          {row === undefined && writesScripts && !copying ? (
            <FormField id={`${formId}-kind`} label={KIND_FIELD} error={undefined}>
              {(control) => (
                <NativeSelect {...control} disabled={locked} {...form.register("kind")}>
                  <option value="steps">{KIND_TEXT.steps}</option>
                  <option value="script">{KIND_TEXT.script}</option>
                </NativeSelect>
              )}
            </FormField>
          ) : null}
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
                  <NativeSelect {...control} disabled={locked} {...form.register("visibility")}>
                    <option value="private">{VISIBILITY_PRIVATE}</option>
                    <option value="shared">{VISIBILITY_SHARED}</option>
                  </NativeSelect>
                ) : (
                  <Input {...control} readOnly value={visibilityText(row.visibility)} />
                )
              }
            </FormField>
          )}
        </>
      }
      sections={
        <>
          {kind === "script" ? (
            <ScriptSections
              form={form}
              view={view}
              templates={row === undefined}
              serverProblem={scriptProblem}
              focusAt={focusAt}
            />
          ) : (
            SECTIONS.map((section) => (
              <StepSection
                key={section}
                form={form}
                section={section}
                view={view}
                full={total >= MODE_MAX_STEPS}
                unknown={0}
                announce={setAnnouncement}
              />
            ))
          )}
          <p role="status" className="sr-only">
            {announcement}
          </p>
        </>
      }
      notes={
        <>
          {formError === undefined ? null : (
            <p ref={inView} role="alert" className="text-sm whitespace-pre-line text-destructive">
              {formError}
            </p>
          )}
          {conflict === null || gone ? null : (
            // One live region: the Alert's own.
            <Alert ref={inView}>
              <AlertDescription className="flex flex-col gap-2 text-foreground">
                <p>{conflict.text}</p>
                {conflict.kind === "others" ? <p>{IN_USE_BY_OTHERS}</p> : null}
                {reloadFailed === null ? null : <p className="text-destructive">{reloadFailed}</p>}
                {conflict.kind === "changed" ? (
                  <Button
                    type="button"
                    variant="outline"
                    className="self-start"
                    disabled={locked}
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
            <div ref={inView} className="flex flex-col gap-2">
              <p role="alert">{LEFT_THE_LIST}</p>
              {keepAsCopy}
            </div>
          ) : null}
          {wait.waiting === null && !wait.ended ? null : (
            <div ref={inView}>
              <RetryWaitMessage wait={wait} />
            </div>
          )}
        </>
      }
      actions={
        <>
          {gone ? null : (
            <Button type="submit" disabled={locked || wait.waiting !== null || !canSave}>
              {SAVE}
            </Button>
          )}
          <Button
            type="button"
            variant="outline"
            disabled={writes.running !== null}
            onClick={() => void navigate({ to: "/modes", replace: true })}
          >
            {CANCEL}
          </Button>
        </>
      }
      apart={
        <>
          {onCopy === undefined || gone ? null : (
            <Button
              type="button"
              variant="outline"
              disabled={locked}
              onClick={() => onCopy(copyOfValues(form.getValues()))}
            >
              {MAKE_A_COPY}
            </Button>
          )}
          {row?.deletable && !gone ? (
            <div className="w-full border-t pt-3">
              <DeleteMode mode={row} writes={writes} onGone={() => setGoneToSave(true)} />
            </div>
          ) : null}
        </>
      }
    />
  );
}
