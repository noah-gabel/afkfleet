// The code step (Plan.md P8.7, ADR-0016): a code from the authenticator app or a recovery
// code, sent without its whitespace. The flow counts wrong codes (3 per step) and keeps the
// step's deadline: a code entered after it is never sent. A code the server finds
// malformed shows next to the field and doesn't count.
import { zodResolver } from "@hookform/resolvers/zod";
import { useState } from "react";
import { useForm } from "react-hook-form";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { FormField } from "../../../components/FormField";
import { RetryWaitMessage } from "../../../components/RetryWaitMessage";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import { useRetryWait } from "../../../hooks/useRetryWait";
import { showFieldErrors } from "../../../lib/formErrors";
import { NEXT_CODE, type Step } from "../authFlowReducer";
import { endsIn, useDeadline } from "../hooks/useDeadline";
import { type CodeValues, codeForm, withoutSpaces } from "../schemas";
import { AuthCard, FormError } from "./AuthCard";

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
  const [formError, setFormError] = useState<string>();
  const wait = useRetryWait();
  const errors = form.formState.errors;

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
        return;
      }
      if (!showFieldErrors(form.setError, error, { code: "code" })) {
        setFormError(describeError(error));
      }
    }
  });

  return (
    <AuthCard title="Two-factor code" intro={step.next ? NEXT_CODE : undefined}>
      <p className="text-sm text-muted-foreground">{endsIn(step.expiresIn)}</p>
      {lastMinute ? (
        <p role="status" className="text-sm font-medium">
          This step ends in under a minute.
        </p>
      ) : null}
      <form className="flex flex-col gap-4" noValidate onSubmit={(event) => void submit(event)}>
        <FormField
          id="login-code"
          label="Code"
          hint="The 6-digit code from your authenticator app, or one of your recovery codes."
          error={errors.code?.message}
        >
          {(control) => (
            <Input {...control} autoComplete="one-time-code" {...form.register("code")} />
          )}
        </FormField>
        {step.message === undefined ? null : (
          <p role="alert" className="text-sm text-destructive">
            {step.message}
          </p>
        )}
        <FormError message={formError} />
        <RetryWaitMessage wait={wait} />
        <div className="flex gap-2">
          <Button
            type="submit"
            disabled={pending || form.formState.isSubmitting || wait.waiting !== null}
          >
            Verify
          </Button>
          <Button type="button" variant="outline" disabled={pending} onClick={onCancel}>
            Cancel
          </Button>
        </div>
      </form>
    </AuthCard>
  );
}
