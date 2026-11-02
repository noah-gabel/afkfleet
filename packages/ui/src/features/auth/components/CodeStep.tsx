// The code step (Plan.md P8.7, P11.11; ADR-0016, ADR-0020, U86, S100): six boxes for a code from the
// authenticator app, which send as they fill, or a text field for a recovery code, sent without its
// whitespace. The flow counts wrong codes (3 per step) and keeps the step's deadline: a code entered
// after it is never sent. A code the server finds malformed shows next to the field and doesn't
// count.
//
// After a wrong code the boxes clear and keep the focus. After a connection failure no try was spent
// and the code may still be good, so it stays, and Verify or Enter sends it again. A 429 clears them,
// as a stale resend could spend a try, and locks them until its wait ends, when nothing is sent by
// itself. The code lives only in this form's one value.
import { zodResolver } from "@hookform/resolvers/zod";
import { useCallback, useEffect, useRef, useState } from "react";
import { useController, useForm } from "react-hook-form";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { FormField } from "../../../components/FormField";
import { RetryWaitMessage } from "../../../components/RetryWaitMessage";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import { useRetryWait } from "../../../hooks/useRetryWait";
import { showFieldErrors } from "../../../lib/formErrors";
import { NEXT_CODE, type Step, triesLeft } from "../authFlowReducer";
import { endsIn, useDeadline } from "../hooks/useDeadline";
import { ALL_SIX, CODE_HINT, type CodeValues, codeForm, withoutSpaces } from "../schemas";
import { AuthCard, FormError } from "./AuthCard";
import { CODE_DIGITS, CodeBoxes } from "./CodeBoxes";

/** The code step. `onSubmit` throws what the form should show. */
export function CodeStep({
  step,
  pending,
  onSubmit,
  onExpire,
  onCancel,
}: {
  step: Extract<Step, { kind: "code" }>;
  pending: boolean;
  onSubmit: (code: string) => Promise<void>;
  onExpire: () => void;
  onCancel: () => void;
}) {
  const lastMinute = useDeadline(step.deadline, onExpire);
  const form = useForm<CodeValues>({
    resolver: zodResolver(codeForm),
    defaultValues: { code: "" },
  });
  const { field: code } = useController({ name: "code", control: form.control });
  const [recovery, setRecovery] = useState(false);
  const [formError, setFormError] = useState<string>();
  const [refused, setRefused] = useState<string>();
  const wait = useRetryWait();
  const errors = form.formState.errors;
  const locked = pending || form.formState.isSubmitting || wait.waiting !== null;
  const control = useRef<HTMLInputElement>(null);

  // The control takes the focus as it mounts, on arrival and at each swap, so typing works at once.
  const attach = useCallback((element: HTMLInputElement | null) => {
    control.current = element;
    element?.focus();
  }, []);

  // A wrong code clears what was typed, in either form.
  const seenTries = useRef(step.triesLeft);
  useEffect(() => {
    if (seenTries.current !== step.triesLeft) {
      seenTries.current = step.triesLeft;
      form.setValue("code", "");
      control.current?.focus();
    }
  }, [step.triesLeft, form]);

  const submit = form.handleSubmit(async (values) => {
    if (wait.waiting !== null) {
      return;
    }
    setFormError(undefined);
    wait.reset();
    if (Date.now() >= step.deadline) {
      onExpire();
      return;
    }
    try {
      await onSubmit(withoutSpaces(values.code));
    } catch (rejection) {
      const error = toApiError(rejection).error;
      if (wait.start(error)) {
        if (!recovery) {
          form.setValue("code", "");
        }
        return;
      }
      if (!showFieldErrors(form.setError, error, { code: "code" })) {
        setFormError(describeError(error));
      }
    }
  });

  /** Swaps the boxes and the text field, holding `text`. */
  const swap = (toRecovery: boolean, text = "") => {
    form.setValue("code", text);
    form.clearErrors("code");
    setRefused(undefined);
    setFormError(undefined);
    setRecovery(toRecovery);
  };

  return (
    <AuthCard
      brand
      title="Two-factor code"
      intro={step.next ? NEXT_CODE : undefined}
      autoFocusHeading={false}
    >
      <p className="text-sm text-muted-foreground">{endsIn(step.expiresIn)}</p>
      {lastMinute ? (
        <p role="status" className="text-sm font-semibold">
          Ends in under a minute.
        </p>
      ) : null}
      <form
        className="flex flex-col gap-3"
        noValidate
        onSubmit={(event) => {
          if (!recovery && code.value.length < CODE_DIGITS) {
            event.preventDefault();
            form.setError("code", { message: ALL_SIX });
            return;
          }
          void submit(event);
        }}
      >
        {recovery ? (
          <FormField
            id="login-recovery"
            label="Recovery code"
            hint="20 characters. Works once."
            error={errors.code?.message}
          >
            {(props) => (
              <Input
                {...props}
                ref={attach}
                autoComplete="off"
                spellCheck={false}
                readOnly={locked}
                className="font-mono"
                value={code.value}
                onChange={code.onChange}
                onBlur={code.onBlur}
                name={code.name}
              />
            )}
          </FormField>
        ) : (
          <FormField id="login-code" label="Code" hint={CODE_HINT} error={errors.code?.message}>
            {(props) => (
              <CodeBoxes
                {...props}
                aria-describedby={[props["aria-describedby"], "login-code-tries"]
                  .filter((id) => id !== undefined)
                  .join(" ")}
                value={code.value}
                locked={locked}
                inputRef={attach}
                onChange={(digits) => {
                  form.clearErrors("code");
                  code.onChange(digits);
                }}
                onFull={(digits) => {
                  form.setValue("code", digits);
                  void submit();
                }}
                onRecovery={(text) => swap(true, text)}
                onRefused={setRefused}
              />
            )}
          </FormField>
        )}
        {refused === undefined ? null : (
          <p role="status" className="text-sm">
            {refused}
          </p>
        )}
        {step.message === undefined ? (
          <p id="login-code-tries" className="text-sm text-muted-foreground">
            {triesLeft(step.triesLeft)}
          </p>
        ) : (
          <p id="login-code-tries" role="alert" className="text-sm text-destructive">
            {step.message}
          </p>
        )}
        <FormError message={formError} />
        <RetryWaitMessage wait={wait} />
        <div className="flex gap-2">
          <Button type="submit" disabled={locked}>
            Verify
          </Button>
          <Button type="button" variant="outline" disabled={pending} onClick={onCancel}>
            Cancel
          </Button>
        </div>
        <Button
          type="button"
          variant="link"
          className="h-auto self-start p-0"
          disabled={locked}
          onClick={() => swap(!recovery)}
        >
          {recovery ? "Use the 6-digit code" : "Use a recovery code"}
        </Button>
      </form>
    </AuthCard>
  );
}
