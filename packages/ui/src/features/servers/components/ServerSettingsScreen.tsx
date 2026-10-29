// One Minecraft server's settings (Plan.md P11.9; ADR-0020, U57, U61, R136, S77): its address
// and its conflict texts, a textarea each (`useFieldArray`), filled from its row in the list,
// each text's lines joined by line breaks. Its chat formats ride along in the form unseen and go
// back as they came, so a save never clears formats set elsewhere (R135, R144); their own
// section comes with H8. Every request is built from the form, never by
// echoing an answer, at the `version` read. Each write goes through `useStepUpAction`: a new
// server's Save opens its own screen, an edit's stays with what the server saved (U61), and
// Delete asks first, then goes back to the list. A 409 `server_settings_changed` shows the
// server's text with "Load again", which reads the row again: only a read that succeeded
// fills the form, and a failed one keeps the notice and the edits and says why. A 404 says the
// settings are gone. Nothing but "Load again" and a save resets the form, so a refetch never
// loses edits.
import { zodResolver } from "@hookform/resolvers/zod";
import { type QueryClient, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { useId, useState } from "react";
import { type Path, useFieldArray, useForm } from "react-hook-form";
import { toast } from "sonner";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { CONFLICT_TEXTS_MAX } from "../../../api/limits";
import { SERVER_SETTINGS_KEY, useMe, useServerSettings } from "../../../api/queries";
import { chatFormats, conflictText, serverAddress } from "../../../api/schemas";
import { z } from "../../../api/zod";
import { FieldMessage } from "../../../components/FieldMessage";
import { FormField } from "../../../components/FormField";
import { RetryWaitMessage } from "../../../components/RetryWaitMessage";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import { Textarea } from "../../../components/ui/textarea";
import type { CommandError } from "../../../generated/CommandError";
import type { ServerSettingsResponse } from "../../../generated/ServerSettingsResponse";
import { useRetryWait } from "../../../hooks/useRetryWait";
import { useAlive } from "../../../hooks/useStepGuard";
import { showFieldErrors } from "../../../lib/formErrors";
import { isStepUpRequired } from "../../../lib/stepUp";
import { StepUpPrompt } from "../../admin/components/StepUpPrompt";
import { useStepUpAction } from "../../admin/hooks/useStepUpAction";
import { serverSettingsAccess } from "../../admin/permissions";
import {
  ADD_SERVER,
  ADD_TEXT,
  AT_MOST_TEXTS,
  added,
  CANCEL,
  CAP_REACHED,
  conflictTextLabel,
  DELETE,
  DELETE_SETTINGS,
  deleted,
  deleteQuestion,
  LOAD_AGAIN,
  LOADING_SERVERS,
  REMOVE,
  SAVE,
  SAVED,
  SERVER_FIELD,
  SERVER_HINT,
  SERVERS_HEADING,
  SETTINGS_GONE,
  serversFailed,
  TEXTS_HEADING,
  TEXTS_HINT,
} from "../texts";

/** The form: the server's address and each text, as `useFieldArray` keeps them. */
const settingsForm = z.object({
  server: serverAddress,
  texts: z
    .array(z.object({ text: conflictText }))
    .refine(
      (texts) => texts.length <= CONFLICT_TEXTS_MAX,
      `must hold at most ${CONFLICT_TEXTS_MAX} texts`,
    ),
  chatFormats,
});

type SettingsValues = z.infer<typeof settingsForm>;

/** The two writes' keys: one at a time per screen. */
const SAVE_KEY = "save";
const DELETE_KEY = "delete";

/** The settings `id`, or a new server's without one. */
export function ServerSettingsScreen({ id }: { id?: string }) {
  const { data: user } = useMe();
  const configure = user !== undefined && serverSettingsAccess(user.role).configure;
  return id === undefined ? (
    <Editor configure={configure} />
  ) : (
    <Existing id={id} configure={configure} />
  );
}

/** Settings `id` from the list: loading, failed, gone, or its editor. */
function Existing({ id, configure }: { id: string; configure: boolean }) {
  const query = useServerSettings();
  const failure = query.error === null ? null : toApiError(query.error).error;
  if (failure !== null && query.data === undefined) {
    return (
      <main className="flex max-w-3xl flex-col gap-4 p-6">
        <p role="alert" className="text-sm text-destructive">
          {serversFailed(describeError(failure))}
        </p>
        <BackLink />
      </main>
    );
  }
  if (query.data === undefined) {
    return (
      <main className="p-6">
        <p className="text-sm text-muted-foreground">{LOADING_SERVERS}</p>
      </main>
    );
  }
  const row = query.data.find((one) => one.id === id);
  return row === undefined ? <Gone /> : <Editor row={row} configure={configure} />;
}

/** Settings that aren't there: never were, or went while their screen was open. */
function Gone() {
  return (
    <main className="flex max-w-3xl flex-col gap-4 p-6">
      <p role="alert">{SETTINGS_GONE}</p>
      <BackLink />
    </main>
  );
}

/** "Minecraft servers": back to the list. */
function BackLink() {
  return (
    <Button asChild variant="link" className="self-start px-0">
      <Link to="/admin/servers">{SERVERS_HEADING}</Link>
    </Button>
  );
}

/** The form's values from `row`, or a new server's with one empty text. */
function valuesOf(row: ServerSettingsResponse | undefined): SettingsValues {
  if (row === undefined) {
    return { server: "", texts: [{ text: "" }], chatFormats: [] };
  }
  return {
    server: row.server,
    texts: row.conflict_texts.map((text) => ({ text: text.lines.join("\n") })),
    chatFormats: [...row.chat_formats],
  };
}

/** Where each of a request's field paths shows on a form of `count` texts. */
function fieldPaths(count: number): Record<string, Path<SettingsValues>> {
  const paths: Record<string, Path<SettingsValues>> = {
    server: "server",
    conflict_texts: "texts",
  };
  for (let index = 0; index < count; index += 1) {
    paths[`conflict_texts[${index}]`] = `texts.${index}.text`;
  }
  return paths;
}

/** `saved` in the list's cache, in its row's place or first; nothing while it's unread. */
function remember(queryClient: QueryClient, saved: ServerSettingsResponse): void {
  queryClient.setQueryData<ServerSettingsResponse[]>(SERVER_SETTINGS_KEY, (rows) => {
    if (rows === undefined) {
      return undefined;
    }
    return rows.some((one) => one.id === saved.id)
      ? rows.map((one) => (one.id === saved.id ? saved : one))
      : [saved, ...rows];
  });
}

/** The editor of `row`, or of a new server's settings without one. */
function Editor({ row, configure }: { row?: ServerSettingsResponse; configure: boolean }) {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const alive = useAlive();
  const wait = useRetryWait();
  const formId = useId();
  // The version the form was filled from: what an edit sends, never the list's newest.
  const [version, setVersion] = useState(row?.version ?? 1);
  const [formError, setFormError] = useState<string>();
  const [stale, setStale] = useState<string | null>(null);
  const [reloadFailed, setReloadFailed] = useState<string | null>(null);
  const [gone, setGone] = useState(false);
  const writes = useStepUpAction();
  const form = useForm<SettingsValues>({
    resolver: zodResolver(settingsForm),
    defaultValues: valuesOf(row),
  });
  const texts = useFieldArray({ control: form.control, name: "texts" });
  const reread = () =>
    void queryClient.invalidateQueries({ queryKey: SERVER_SETTINGS_KEY, exact: true });

  /** What a refusal other than the step-up's shows: a wait, a field's text, or a line. */
  function show(error: CommandError): void {
    if (wait.start(error)) {
      return;
    }
    const details = error.api;
    if (details?.status === 409 && details.code === "server_settings_changed") {
      setStale(details.message);
    } else if (details?.status === 409 && row === undefined) {
      setFormError(CAP_REACHED);
    } else if (details?.status === 404) {
      setGone(true);
      reread();
    } else if (!showFieldErrors(form.setError, error, fieldPaths(form.getValues("texts").length))) {
      setFormError(describeError(error));
    }
  }

  const submit = form.handleSubmit((values) => {
    if (wait.waiting !== null) {
      return;
    }
    setFormError(undefined);
    setStale(null);
    setReloadFailed(null);
    wait.reset();
    const server = values.server;
    const conflictTexts = values.texts.map((entry) => entry.text);
    const formats = values.chatFormats;
    void writes.attempt({
      key: SAVE_KEY,
      run: async () => {
        let saved: ServerSettingsResponse;
        try {
          saved =
            row === undefined
              ? await api.createServerSettings({
                  server,
                  conflict_texts: conflictTexts,
                  chat_formats: formats,
                })
              : await api.updateServerSettings(row.id, {
                  version,
                  server,
                  conflict_texts: conflictTexts,
                  chat_formats: formats,
                });
        } catch (rejection) {
          const error = toApiError(rejection).error;
          if (isStepUpRequired(error)) {
            throw rejection;
          }
          if (alive.current) {
            show(error);
          }
          return;
        }
        remember(queryClient, saved);
        reread();
        if (!alive.current) {
          return;
        }
        if (row === undefined) {
          toast.success(added(saved.server));
          void navigate({ to: "/admin/servers/$id", params: { id: saved.id }, replace: true });
        } else {
          toast.success(SAVED);
          setVersion(saved.version);
          form.reset(valuesOf(saved));
        }
      },
    });
  });

  /**
   * Reads the row again after a 409 and fills the form from it; a row gone shows so. A read
   * that fails changes nothing but saying why: the notice and the edits stay, and the list's
   * cache still holds the old row, which must never be shown as current.
   */
  async function loadAgain(): Promise<void> {
    if (row === undefined) {
      return;
    }
    setReloadFailed(null);
    try {
      await queryClient.refetchQueries(
        { queryKey: SERVER_SETTINGS_KEY, exact: true },
        { throwOnError: true },
      );
    } catch (rejection) {
      if (alive.current) {
        setReloadFailed(serversFailed(describeError(toApiError(rejection).error)));
      }
      return;
    }
    if (!alive.current) {
      return;
    }
    setStale(null);
    const fresh = queryClient
      .getQueryData<ServerSettingsResponse[]>(SERVER_SETTINGS_KEY)
      ?.find((one) => one.id === row.id);
    if (fresh !== undefined) {
      setVersion(fresh.version);
      form.reset(valuesOf(fresh));
    }
  }

  if (gone) {
    return <Gone />;
  }
  // While a write runs or the password is asked, what's sent is what's on the screen: the
  // fields and the buttons are locked.
  const locked = writes.prompt !== null || writes.running !== null;
  const readOnly = locked || !configure;
  const errors = form.formState.errors;
  const full = texts.fields.length >= CONFLICT_TEXTS_MAX;
  const listError = errors.texts?.message ?? errors.texts?.root?.message;
  return (
    <main className="flex max-w-3xl flex-col gap-6 p-6">
      <h1 className="text-2xl font-semibold tracking-tight">{row?.server ?? ADD_SERVER}</h1>
      <form className="flex flex-col gap-6" noValidate onSubmit={(event) => void submit(event)}>
        <FormField
          id={`${formId}-server`}
          label={SERVER_FIELD}
          hint={SERVER_HINT}
          error={errors.server?.message}
        >
          {(control) => (
            <Input
              {...control}
              autoComplete="off"
              readOnly={readOnly}
              {...form.register("server")}
            />
          )}
        </FormField>
        <fieldset
          className="flex flex-col gap-4"
          aria-describedby={
            listError === undefined
              ? `${formId}-texts-hint`
              : `${formId}-texts-hint ${formId}-texts-error`
          }
        >
          <legend className="mb-2 text-lg font-semibold">{TEXTS_HEADING}</legend>
          <p id={`${formId}-texts-hint`} className="text-sm text-muted-foreground">
            {TEXTS_HINT}
          </p>
          {texts.fields.map((field, index) => (
            <div key={field.id} className="flex flex-col gap-2">
              <FormField
                id={`${formId}-text-${field.id}`}
                label={conflictTextLabel(index + 1)}
                error={errors.texts?.[index]?.text?.message}
              >
                {(control) => (
                  <Textarea
                    {...control}
                    readOnly={readOnly}
                    {...form.register(`texts.${index}.text`)}
                  />
                )}
              </FormField>
              {configure ? (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  className="self-start"
                  disabled={locked}
                  aria-describedby={`${formId}-text-${field.id}-label`}
                  onClick={() => texts.remove(index)}
                >
                  {REMOVE}
                </Button>
              ) : null}
            </div>
          ))}
          <FieldMessage id={`${formId}-texts-error`} message={listError} />
          {configure ? (
            <div className="flex flex-wrap items-center gap-3">
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={locked || full}
                onClick={() => texts.append({ text: "" })}
              >
                {ADD_TEXT}
              </Button>
              {full ? <span className="text-sm text-muted-foreground">{AT_MOST_TEXTS}</span> : null}
            </div>
          ) : null}
        </fieldset>
        {formError === undefined ? null : (
          <p role="alert" className="text-sm text-destructive">
            {formError}
          </p>
        )}
        {stale === null ? null : (
          // One live region: the Alert's own.
          <Alert>
            <AlertDescription className="flex flex-col gap-2 text-foreground">
              <p>{stale}</p>
              {reloadFailed === null ? null : <p className="text-destructive">{reloadFailed}</p>}
              <Button
                type="button"
                variant="outline"
                className="self-start"
                onClick={() => void loadAgain()}
              >
                {LOAD_AGAIN}
              </Button>
            </AlertDescription>
          </Alert>
        )}
        <RetryWaitMessage wait={wait} />
        <div className="flex gap-2">
          {configure ? (
            <Button type="submit" disabled={locked || wait.waiting !== null}>
              {SAVE}
            </Button>
          ) : null}
          <Button
            type="button"
            variant="outline"
            disabled={writes.running !== null}
            onClick={() => {
              writes.cancel();
              void navigate({ to: "/admin/servers" });
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
      {row !== undefined && configure ? (
        <DeleteAction
          row={row}
          writes={writes}
          onGone={() => {
            setGone(true);
            reread();
          }}
        />
      ) : null}
      <BackLink />
    </main>
  );
}

/** Delete, with its question, prompt and failure; afterwards, back to the list. */
function DeleteAction({
  row,
  writes,
  onGone,
}: {
  row: ServerSettingsResponse;
  writes: ReturnType<typeof useStepUpAction>;
  onGone: () => void;
}) {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const alive = useAlive();
  const [asking, setAsking] = useState(false);
  const [failure, setFailure] = useState<string>();
  const busy = writes.prompt !== null || writes.running !== null;

  function remove() {
    setAsking(false);
    setFailure(undefined);
    void writes.attempt({
      key: DELETE_KEY,
      run: async () => {
        try {
          await api.deleteServerSettings(row.id);
        } catch (rejection) {
          const error = toApiError(rejection).error;
          if (isStepUpRequired(error)) {
            throw rejection;
          }
          if (!alive.current) {
            return;
          }
          if (error.api?.status === 404) {
            onGone();
          } else {
            setFailure(describeError(error));
          }
          return;
        }
        if (alive.current) {
          toast.success(deleted(row.server));
          await navigate({ to: "/admin/servers" });
        }
        queryClient.setQueryData<ServerSettingsResponse[]>(SERVER_SETTINGS_KEY, (rows) =>
          rows?.filter((one) => one.id !== row.id),
        );
        void queryClient.invalidateQueries({ queryKey: SERVER_SETTINGS_KEY, exact: true });
      },
    });
  }

  return (
    <div className="flex flex-col gap-2">
      <Button
        type="button"
        variant="destructive"
        className="self-start"
        disabled={busy}
        onClick={() => setAsking(true)}
      >
        {DELETE}
      </Button>
      {asking ? (
        <Alert>
          <AlertDescription className="flex flex-col gap-2 text-foreground">
            <p>{deleteQuestion(row.server)}</p>
            <div className="flex gap-2">
              <Button type="button" variant="destructive" disabled={busy} onClick={remove}>
                {DELETE_SETTINGS}
              </Button>
              <Button type="button" variant="outline" onClick={() => setAsking(false)}>
                {CANCEL}
              </Button>
            </div>
          </AlertDescription>
        </Alert>
      ) : null}
      {writes.prompt === DELETE_KEY ? (
        <StepUpPrompt
          id="step-up-delete"
          pending={writes.running !== null}
          onConfirm={writes.confirm}
          onCancel={writes.cancel}
        />
      ) : null}
      {writes.failure?.key === DELETE_KEY ? (
        <p role="alert" className="text-sm text-destructive">
          {writes.failure.message}
        </p>
      ) : null}
      {failure === undefined ? null : (
        <p role="alert" className="text-sm text-destructive">
          {failure}
        </p>
      )}
    </div>
  );
}
