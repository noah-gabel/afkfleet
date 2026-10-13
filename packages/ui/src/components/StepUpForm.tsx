// "Confirm it's you" (Plan.md §7.2, P8.7, P8.8, ADR-0016): the password again, for a
// step-up. The profile's flows start with it, and the admin screens show it inline when the
// server answers `step_up_required`. The password lives only in this form, which empties its
// field after every answer, and goes to `onConfirm`, a direct call (never a query or a
// mutation, CLAUDE.md). A wrong one is the server's 403 `forbidden`; it counts toward the
// lockout like a wrong login. A 429's wait (the Login bucket, or a locked name) counts down,
// Continue waiting with it (useRetryWait).
import { zodResolver } from "@hookform/resolvers/zod";
import { useState } from "react";
import { useForm } from "react-hook-form";

import { toApiError } from "../api/ApiError";
import { describeError } from "../api/describeError";
import { password } from "../api/schemas";
import { z } from "../api/zod";
import { useRetryWait } from "../hooks/useRetryWait";
import { showFieldErrors } from "../lib/formErrors";
import { isWrongPassword, WRONG_PASSWORD } from "../lib/stepUp";
import { FormField } from "./FormField";
import { PasswordInput } from "./PasswordInput";
import { RetryWaitMessage } from "./RetryWaitMessage";
import { Button } from "./ui/button";

/** The form: a password that isn't blank, within the server's limit. */
const stepUpForm = z.object({
  password: z
    .string()
    .refine((text) => text !== "", "Enter your password.")
    .pipe(password),
});

type StepUpValues = z.infer<typeof stepUpForm>;

/** The password form. `onConfirm` throws what the form should show. */
export function StepUpForm({
  id,
  pending,
  onConfirm,
  onCancel,
}: {
  /** The field's id, unique on the page. */
  id: string;
  /** The call is running: Continue and Cancel wait. */
  pending: boolean;
  onConfirm: (password: string) => Promise<void>;
  onCancel: () => void;
}) {
  const form = useForm<StepUpValues>({
    resolver: zodResolver(stepUpForm),
    defaultValues: { password: "" },
  });
  const [formError, setFormError] = useState<string>();
  const wait = useRetryWait();

  const submit = form.handleSubmit(async (values) => {
    if (wait.waiting !== null) {
      return;
    }
    setFormError(undefined);
    wait.reset();
    try {
      await onConfirm(values.password);
      form.reset({ password: "" });
    } catch (rejection) {
      const error = toApiError(rejection).error;
      form.reset({ password: "" });
      if (wait.start(error)) {
        return;
      }
      if (isWrongPassword(error)) {
        setFormError(WRONG_PASSWORD);
      } else if (!showFieldErrors(form.setError, error, { password: "password" })) {
        setFormError(describeError(error));
      }
    }
  });

  return (
    <form className="flex flex-col gap-4" noValidate onSubmit={(event) => void submit(event)}>
      <FormField id={id} label="Current password" error={form.formState.errors.password?.message}>
        {(control) => (
          <PasswordInput
            {...control}
            toggleLabel="Show password"
            autoComplete="current-password"
            {...form.register("password")}
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
        <Button
          type="submit"
          disabled={pending || form.formState.isSubmitting || wait.waiting !== null}
        >
          Continue
        </Button>
        <Button type="button" variant="outline" disabled={pending} onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </form>
  );
}
