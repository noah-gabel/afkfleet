// The password change's second step (Plan.md P8.7): the new password, typed twice, before
// the step-up's 5 minutes end (counted from when it was sent). The form checks the length
// and the repeat; the server checks the strength, and every reason it gives shows at the
// field. A password entered after the deadline is never sent. A 429's wait counts down, the
// button waiting with it (useRetryWait).
import { zodResolver } from "@hookform/resolvers/zod";
import { useState } from "react";
import { useForm } from "react-hook-form";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { FormField } from "../../../components/FormField";
import { PasswordInput } from "../../../components/PasswordInput";
import { RetryWaitMessage } from "../../../components/RetryWaitMessage";
import { Button } from "../../../components/ui/button";
import { useRetryWait } from "../../../hooks/useRetryWait";
import { showFieldErrors } from "../../../lib/formErrors";
import { AuthCard, FormError } from "../../auth/components/AuthCard";
import { endsIn, useDeadline } from "../../auth/hooks/useDeadline";
import type { ProfileStep } from "../profileFlowReducer";
import { type NewPasswordValues, newPasswordForm } from "../schemas";

/** The new password step. `onSubmit` throws what the form should show. */
export function NewPasswordStep({
  step,
  pending,
  onSubmit,
  onExpire,
  onCancel,
}: {
  step: Extract<ProfileStep, { kind: "newPassword" }>;
  pending: boolean;
  onSubmit: (password: string) => Promise<void>;
  onExpire: () => void;
  onCancel: () => void;
}) {
  const lastMinute = useDeadline(step.deadline, onExpire);
  const form = useForm<NewPasswordValues>({
    resolver: zodResolver(newPasswordForm),
    defaultValues: { password: "", repeat: "" },
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
      await onSubmit(values.password);
    } catch (rejection) {
      const error = toApiError(rejection).error;
      if (wait.start(error)) {
        return;
      }
      if (!showFieldErrors(form.setError, error, { new_password: "password" })) {
        setFormError(describeError(error));
      }
    }
  });

  return (
    <AuthCard
      title="Choose a new password"
      intro="12 to 128 characters. Every other session of your account is logged out."
    >
      <p className="text-sm text-muted-foreground">{endsIn(step.expiresIn)}</p>
      {lastMinute ? (
        <p role="status" className="text-sm font-medium">
          This step ends in under a minute.
        </p>
      ) : null}
      <form className="flex flex-col gap-4" noValidate onSubmit={(event) => void submit(event)}>
        <FormField id="new-password" label="New password" error={errors.password?.message}>
          {(control) => (
            <PasswordInput
              {...control}
              toggleLabel="Show new password"
              autoComplete="new-password"
              {...form.register("password")}
            />
          )}
        </FormField>
        <FormField
          id="new-password-repeat"
          label="Repeat the new password"
          error={errors.repeat?.message}
        >
          {(control) => (
            <PasswordInput
              {...control}
              toggleLabel="Show repeated password"
              autoComplete="new-password"
              {...form.register("repeat")}
            />
          )}
        </FormField>
        <FormError message={formError} />
        <RetryWaitMessage wait={wait} />
        <div className="flex gap-2">
          <Button
            type="submit"
            disabled={pending || form.formState.isSubmitting || wait.waiting !== null}
          >
            Change password
          </Button>
          <Button type="button" variant="outline" disabled={pending} onClick={onCancel}>
            Cancel
          </Button>
        </div>
      </form>
    </AuthCard>
  );
}
