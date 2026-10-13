// The register form (Plan.md P8.7, ADR-0016): the invite code (trimmed of what a chat app
// adds around it), a username, and the new password typed twice, each with its own "show"
// toggle. The repeat is checked here and never sent. The server's refusals (an invite it
// can't use, a taken name, a weak password) show next to their fields; a 429's wait counts
// down, the button waiting with it (useRetryWait).
import { zodResolver } from "@hookform/resolvers/zod";
import { Link } from "@tanstack/react-router";
import { useState } from "react";
import { useForm } from "react-hook-form";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { FormField } from "../../../components/FormField";
import { PasswordInput } from "../../../components/PasswordInput";
import { RetryWaitMessage } from "../../../components/RetryWaitMessage";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import { useRetryWait } from "../../../hooks/useRetryWait";
import { showFieldErrors } from "../../../lib/formErrors";
import { type RegisterInput, type RegisterValues, registerForm } from "../schemas";
import { AuthCard, FormError } from "./AuthCard";

/** The register form. `onSubmit` throws what the form should show. */
export function RegisterForm({
  origin,
  pending,
  onSubmit,
}: {
  origin: string | undefined;
  pending: boolean;
  onSubmit: (values: RegisterValues) => Promise<void>;
}) {
  const form = useForm<RegisterInput, unknown, RegisterValues>({
    resolver: zodResolver(registerForm),
    defaultValues: { invite: "", username: "", password: "", repeat: "" },
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
    try {
      await onSubmit(values);
    } catch (rejection) {
      const error = toApiError(rejection).error;
      if (wait.start(error)) {
        return;
      }
      const paths = { invite: "invite", username: "username", password: "password" } as const;
      if (!showFieldErrors(form.setError, error, paths)) {
        setFormError(describeError(error));
      }
    }
  });

  return (
    <AuthCard
      title="Register"
      intro={origin === undefined ? undefined : `Registering on ${origin}`}
    >
      <form className="flex flex-col gap-4" noValidate onSubmit={(event) => void submit(event)}>
        <FormField
          id="register-invite"
          label="Invite code"
          hint="The code from the invite you were sent."
          error={errors.invite?.message}
        >
          {(control) => <Input {...control} autoComplete="off" {...form.register("invite")} />}
        </FormField>
        <FormField id="register-username" label="Username" error={errors.username?.message}>
          {(control) => (
            <Input {...control} autoComplete="username" {...form.register("username")} />
          )}
        </FormField>
        <FormField id="register-password" label="Password" error={errors.password?.message}>
          {(control) => (
            <PasswordInput
              {...control}
              toggleLabel="Show password"
              autoComplete="new-password"
              {...form.register("password")}
            />
          )}
        </FormField>
        <FormField id="register-repeat" label="Repeat password" error={errors.repeat?.message}>
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
        <Button
          type="submit"
          disabled={pending || form.formState.isSubmitting || wait.waiting !== null}
        >
          Register
        </Button>
      </form>
      <Link to="/login" disabled={pending} className="text-sm underline underline-offset-4">
        Back to login
      </Link>
    </AuthCard>
  );
}
