// A bot's Settings (Plan.md P11.9; ADR-0019, S47; ADR-0020, U40, U41, U48, S67, S68, S69,
// R102, R105): what the viewer may change, and nothing else; the rest stays among the facts.
//
// - **The mode** (Control and above): a native drop-down that switches at once, "Switching…"
//   meanwhile. Its choices are the modes the viewer may choose by each one's
//   `unlisted_command`; the bot's mode is its value, by name, or "A mode you can't see" when
//   the list lacks it, and a choice only when the viewer may choose it. The current mode is
//   never sent. A 403 says only a manager may choose that mode and reads the bot and the
//   modes again; a 422 shows its field text and reads the modes again; a 404 reads the bot
//   again, as the controls do.
// - **The server** (Manage): the address with Change, which opens a field in place; Save asks
//   for the password when the server wants it (useStepUpAction), and spends the bot's limit,
//   whose 429 counts down on Save. While a save runs or the password is asked, the field and
//   Save are locked, and while a save runs, Cancel is.
// Every answer reads the bot again rather than writing over the cache (R102).
import { zodResolver } from "@hookform/resolvers/zod";
import { useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import { useForm } from "react-hook-form";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { botKey, MODES_LIST_KEY, useModes } from "../../../api/queries";
import { botPatchRequest } from "../../../api/schemas";
import { FormField } from "../../../components/FormField";
import { RetryWaitMessage } from "../../../components/RetryWaitMessage";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import type { BotPatchRequest } from "../../../generated/BotPatchRequest";
import type { BotResponse } from "../../../generated/BotResponse";
import type { CommandError } from "../../../generated/CommandError";
import { useRetryWait } from "../../../hooks/useRetryWait";
import { useAlive } from "../../../hooks/useStepGuard";
import { fieldErrorsOf, showFieldErrors } from "../../../lib/formErrors";
import { isStepUpRequired } from "../../../lib/stepUp";
import { StepUpPrompt } from "../../admin/components/StepUpPrompt";
import { useStepUpAction } from "../../admin/hooks/useStepUpAction";
import { modeLabel, modesOf } from "../facts";
import { type BotActions, botActions, mayChoose } from "../permissions";
import { HIDDEN_MODE, modeName, SETTINGS } from "../texts";

/** The selector's value for a current mode the viewer can't see. */
const UNSEEN = "";

/** `bot`'s Settings, when the viewer may change any of them. */
export function BotSettings({ bot }: { bot: BotResponse }) {
  const headingId = useId();
  const actions = botActions(bot.account.level);
  if (!actions.chooseMode && !actions.configure) {
    return null;
  }
  return (
    <section aria-labelledby={headingId} className="flex flex-col gap-4">
      <h2 id={headingId} className="text-lg font-semibold">
        {SETTINGS.heading}
      </h2>
      {actions.chooseMode ? <ModeSetting bot={bot} actions={actions} /> : null}
      {actions.configure ? <ServerSetting bot={bot} /> : null}
    </section>
  );
}

/** The bot's mode, chosen at once from the modes the viewer may choose. */
function ModeSetting({ bot, actions }: { bot: BotResponse; actions: BotActions }) {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const modes = useModes();
  const alive = useAlive();
  const wait = useRetryWait();
  const id = useId();
  const [picked, setPicked] = useState<string | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const known = modesOf(modes);

  if (typeof known === "string") {
    return (
      <div className="flex flex-col gap-1.5 text-sm">
        <span className="font-medium">{SETTINGS.mode}</span>
        <span className="text-muted-foreground">{modeLabel(bot.mode_id, known)}</span>
      </div>
    );
  }

  const listed = known.some((mode) => mode.id === bot.mode_id);
  const shown = known.filter((mode) => mode.id === bot.mode_id || mayChoose(mode, actions));

  /** What a refusal of the switch says; the server's 403 and 422 also read again. */
  function refused(error: CommandError): string {
    const status = error.api?.status;
    if (status === 403) {
      void queryClient.invalidateQueries({ queryKey: botKey(bot.id), exact: true });
      void queryClient.invalidateQueries({ queryKey: MODES_LIST_KEY, exact: true });
      return SETTINGS.modeForbidden;
    }
    if (status === 422) {
      void queryClient.invalidateQueries({ queryKey: MODES_LIST_KEY, exact: true });
      const field = fieldErrorsOf(error).find((one) => one.path === "mode_id");
      return field?.message ?? describeError(error);
    }
    if (status === 404) {
      // As the controls do: the read shows the screen's own 404.
      void queryClient.invalidateQueries({ queryKey: botKey(bot.id), exact: true });
    }
    return describeError(error);
  }

  async function choose(modeId: string): Promise<void> {
    if (modeId === bot.mode_id || modeId === UNSEEN || picked !== null || wait.waiting !== null) {
      return;
    }
    setPicked(modeId);
    setFailure(null);
    wait.reset();
    try {
      await api.setBotMode(bot.id, { mode_id: modeId });
      await queryClient.invalidateQueries({ queryKey: botKey(bot.id), exact: true });
    } catch (rejection) {
      const error = toApiError(rejection).error;
      if (!wait.start(error) && alive.current) {
        setFailure(refused(error));
      }
    } finally {
      if (alive.current) {
        setPicked(null);
      }
    }
  }

  return (
    <div className="flex flex-col gap-1.5">
      <label htmlFor={id} className="text-sm font-medium">
        {SETTINGS.mode}
      </label>
      <div className="flex flex-wrap items-center gap-3">
        <select
          id={id}
          className="h-9 rounded-md border border-input bg-transparent px-3 text-sm"
          value={picked ?? (listed ? bot.mode_id : UNSEEN)}
          disabled={picked !== null || wait.waiting !== null}
          onChange={(event) => {
            void choose(event.target.value);
          }}
        >
          {listed ? null : (
            <option value={UNSEEN} disabled>
              {HIDDEN_MODE}
            </option>
          )}
          {shown.map((mode) => (
            <option key={mode.id} value={mode.id} disabled={!mayChoose(mode, actions)}>
              {modeName(mode)}
            </option>
          ))}
        </select>
        {picked === null ? null : (
          <span role="status" className="text-sm text-muted-foreground">
            {SETTINGS.switching}
          </span>
        )}
      </div>
      {failure === null ? null : (
        <p role="alert" className="text-sm text-destructive">
          {failure}
        </p>
      )}
      <RetryWaitMessage wait={wait} />
    </div>
  );
}

/** The bot's server: its address, changed in place after the password (Manage). */
function ServerSetting({ bot }: { bot: BotResponse }) {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const wait = useRetryWait();
  const key = "server";
  const fieldId = useId();
  const [editing, setEditing] = useState(false);
  const [formError, setFormError] = useState<string>();
  const writes = useStepUpAction({
    onRefused: () => {
      void queryClient.invalidateQueries({ queryKey: botKey(bot.id), exact: true });
    },
  });
  const form = useForm<BotPatchRequest>({
    resolver: zodResolver(botPatchRequest),
    defaultValues: { server_address: bot.server_address ?? "" },
  });

  /** What a refusal other than the step-up's shows: a wait, a field's text, or a line. */
  function show(error: CommandError): void {
    if (wait.start(error)) {
      return;
    }
    if (!showFieldErrors(form.setError, error, { server_address: "server_address" })) {
      setFormError(describeError(error));
    }
    if (error.api?.status === 403 || error.api?.status === 404) {
      void queryClient.invalidateQueries({ queryKey: botKey(bot.id), exact: true });
    }
  }

  const submit = form.handleSubmit((values) => {
    if (wait.waiting !== null) {
      return;
    }
    setFormError(undefined);
    wait.reset();
    void writes.attempt({
      key,
      run: async () => {
        try {
          await api.patchBot(bot.id, values);
        } catch (rejection) {
          const error = toApiError(rejection).error;
          if (isStepUpRequired(error)) {
            throw rejection;
          }
          show(error);
          return;
        }
        setEditing(false);
        await queryClient.invalidateQueries({ queryKey: botKey(bot.id), exact: true });
      },
    });
  });

  if (!editing) {
    return (
      <div className="flex flex-col gap-1.5 text-sm">
        <span className="font-medium">{SETTINGS.server}</span>
        <div className="flex flex-wrap items-center gap-3">
          <span className={bot.server_address === undefined ? "text-muted-foreground" : ""}>
            {bot.server_address ?? SETTINGS.notSet}
          </span>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => {
              form.reset({ server_address: bot.server_address ?? "" });
              setFormError(undefined);
              setEditing(true);
            }}
          >
            {bot.server_address === undefined ? SETTINGS.set : SETTINGS.change}
          </Button>
        </div>
      </div>
    );
  }

  // While a save runs or the password is asked, the write that waits is the one shown: the
  // field and Save are locked, so what's sent after the step-up is what's on the screen.
  const asking = writes.prompt === key;
  const locked = asking || writes.running !== null;
  return (
    <div className="flex flex-col gap-3">
      <form className="flex flex-col gap-3" noValidate onSubmit={(event) => void submit(event)}>
        <FormField
          id={fieldId}
          label={SETTINGS.address}
          hint={SETTINGS.addressHint}
          error={form.formState.errors.server_address?.message}
        >
          {(control) => (
            <Input
              {...control}
              autoComplete="off"
              readOnly={locked}
              {...form.register("server_address")}
            />
          )}
        </FormField>
        {formError === undefined ? null : (
          <p role="alert" className="text-sm text-destructive">
            {formError}
          </p>
        )}
        <RetryWaitMessage wait={wait} />
        <div className="flex gap-2">
          <Button type="submit" size="sm" disabled={locked || wait.waiting !== null}>
            {SETTINGS.save}
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={writes.running !== null}
            onClick={() => {
              writes.cancel();
              setEditing(false);
            }}
          >
            {SETTINGS.cancel}
          </Button>
        </div>
      </form>
      {/* Outside the form: "Confirm it's you" is a form of its own. */}
      {writes.prompt === key ? (
        <StepUpPrompt
          id={`step-up-${key}`}
          pending={writes.running !== null}
          onConfirm={writes.confirm}
          onCancel={writes.cancel}
        />
      ) : null}
      {writes.failure?.key === key ? (
        <p role="alert" className="text-sm text-destructive">
          {writes.failure.message}
        </p>
      ) : null}
    </div>
  );
}
