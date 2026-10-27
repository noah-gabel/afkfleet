// A bot's send box (Plan.md P11.9; ADR-0020, U34, U45, U48, S4, S5, S8, S70, R115–R119),
// under its chat, for a viewer who may chat (Control and above).
//
// - **Each send is a direct call** (U34): a typed command may hold a password, so the line
//   lives only in this form's state, never in a cache, and goes when it's sent.
// - **While it runs** the box is locked, with a spinner; the server answers within its 6 s
//   wait (S5), so no timer here. On 200 or 202 it empties, and the answer links the line to
//   its request in the chat's outcomes, sent or pending from the loss count the send began
//   at (S70, R116); without a live watch the chat is read once more (R115).
// - **A command off the allowlist** (`command_needs_step_up`, S4) goes, as typed, to
//   `/command` (S8) through the step-up: the box stays locked while the password is asked,
//   so what's sent is what's shown (R118).
// - **A refusal shows by the box, and the text stays** (U45, U48): a 409's or a 403's own
//   message (`chat_unavailable` offers no "try again"), a 422's at the field, a 429's
//   countdown on Send; after a 403 or a 404 the bot is read again. The server judges
//   everything else (R117).
import { zodResolver } from "@hookform/resolvers/zod";
import { useQueryClient } from "@tanstack/react-query";
import { LoaderCircleIcon } from "lucide-react";
import { useId, useState } from "react";
import { useForm } from "react-hook-form";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { botKey, chatKey, recordAnswer } from "../../../api/queries";
import { chatSendRequest } from "../../../api/schemas";
import { useAppStores } from "../../../app/context";
import { RetryWaitMessage } from "../../../components/RetryWaitMessage";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import type { BotResponse } from "../../../generated/BotResponse";
import type { ChatSendRequest } from "../../../generated/ChatSendRequest";
import type { ChatSendResponse } from "../../../generated/ChatSendResponse";
import type { CommandError } from "../../../generated/CommandError";
import { useRetryWait } from "../../../hooks/useRetryWait";
import { useAlive } from "../../../hooks/useStepGuard";
import { showFieldErrors } from "../../../lib/formErrors";
import { isStepUpRequired } from "../../../lib/stepUp";
import { StepUpPrompt } from "../../admin/components/StepUpPrompt";
import { useStepUpAction } from "../../admin/hooks/useStepUpAction";
import { CHAT } from "../texts";

/** The key of the command that waits for the password. */
const COMMAND = "command";

/** `bot`'s send box. */
export function ChatSend({ bot }: { bot: BotResponse }) {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const { live } = useAppStores();
  const alive = useAlive();
  const wait = useRetryWait();
  const fieldId = useId();
  const errorId = useId();
  const [sending, setSending] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const commands = useStepUpAction();
  const form = useForm<ChatSendRequest>({
    resolver: zodResolver(chatSendRequest),
    defaultValues: { message: "" },
  });
  const asking = commands.prompt === COMMAND;
  const locked = sending || commands.running !== null || asking;

  /** A send's answer: the line linked to its request, the box emptied (S70, U45). */
  function answered(answer: ChatSendResponse, since: number): void {
    recordAnswer(queryClient, bot.id, answer, since);
    if (!live.getState().connected) {
      void queryClient.invalidateQueries({ queryKey: chatKey(bot.id), exact: true });
    }
    if (alive.current) {
      form.reset({ message: "" });
    }
  }

  /** What a refusal shows by the box (U48); the text stays. */
  function refused(error: CommandError): void {
    if (!alive.current || wait.start(error)) {
      return;
    }
    if (showFieldErrors(form.setError, error, { message: "message" })) {
      return;
    }
    const status = error.api?.status;
    setFailure(
      error.api !== undefined && (status === 409 || status === 403)
        ? error.api.message
        : describeError(error),
    );
    if (status === 403 || status === 404) {
      void queryClient.invalidateQueries({ queryKey: botKey(bot.id), exact: true });
    }
  }

  /** `message` as a command, after the step-up the server asks for (S4, S8). */
  function command(message: string): void {
    void commands.attempt({
      key: COMMAND,
      run: async () => {
        const since = live.getState().losses;
        try {
          answered(await api.sendCommand(bot.id, { message }), since);
        } catch (rejection) {
          const error = toApiError(rejection).error;
          if (isStepUpRequired(error)) {
            throw rejection;
          }
          refused(error);
        }
      },
    });
  }

  const submit = form.handleSubmit(async ({ message }) => {
    if (locked || wait.waiting !== null) {
      return;
    }
    setFailure(null);
    wait.reset();
    const since = live.getState().losses;
    setSending(true);
    try {
      answered(await api.sendChat(bot.id, { message }), since);
    } catch (rejection) {
      const error = toApiError(rejection).error;
      if (error.api?.code === "command_needs_step_up") {
        command(message);
      } else {
        refused(error);
      }
    } finally {
      if (alive.current) {
        setSending(false);
      }
    }
  });

  const fieldError = form.formState.errors.message?.message;
  const busy = sending || commands.running !== null;
  return (
    <div className="flex flex-col gap-2">
      <form
        className="flex items-start gap-2"
        noValidate
        onSubmit={(event) => {
          void submit(event);
        }}
      >
        <Input
          id={fieldId}
          aria-label={CHAT.placeholder}
          placeholder={CHAT.placeholder}
          autoComplete="off"
          readOnly={locked}
          aria-invalid={fieldError === undefined ? undefined : true}
          aria-describedby={fieldError === undefined ? undefined : errorId}
          {...form.register("message")}
        />
        <Button type="submit" disabled={locked || wait.waiting !== null}>
          {busy ? (
            <LoaderCircleIcon role="img" aria-label={CHAT.sending} className="animate-spin" />
          ) : null}
          {CHAT.send}
        </Button>
      </form>
      {fieldError === undefined ? null : (
        <p id={errorId} className="text-sm text-destructive">
          {fieldError}
        </p>
      )}
      {failure === null ? null : (
        <p role="alert" className="text-sm text-destructive">
          {failure}
        </p>
      )}
      {commands.failure?.key === COMMAND ? (
        <p role="alert" className="text-sm text-destructive">
          {commands.failure.message}
        </p>
      ) : null}
      <RetryWaitMessage wait={wait} />
      {asking ? (
        <StepUpPrompt
          id={`step-up-${COMMAND}`}
          text={CHAT.commandStepUp}
          pending={commands.running !== null}
          onConfirm={commands.confirm}
          onCancel={commands.cancel}
        />
      ) : null}
    </div>
  );
}
