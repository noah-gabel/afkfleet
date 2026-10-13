// The password step (Plan.md P8.7): username and password, checked by the API's own
// schema before anything is sent. What the server refuses shows next to its field, or above
// the button with describeError's text (a 429 with its wait). A notice says why the flow
// came back here; one about a session the flow can't use offers "Log out".
import { zodResolver } from "@hookform/resolvers/zod";
import { Link } from "@tanstack/react-router";
import { useState } from "react";
import { useForm } from "react-hook-form";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { FormField } from "../../../components/FormField";
import { PasswordInput } from "../../../components/PasswordInput";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import { showFieldErrors } from "../../../lib/formErrors";
import type { Notice } from "../authFlowReducer";
import { type LoginValues, loginForm } from "../schemas";
import { AuthCard, FormError } from "./AuthCard";

/** The login form. `onSubmit` throws what the form should show. */
export function LoginForm({
  origin,
  username,
  notice,
  onSubmit,
  onLogOut,
}: {
  origin: string | undefined;
  username: string;
  notice: Notice | undefined;
  onSubmit: (values: LoginValues) => Promise<void>;
  onLogOut: () => void;
}) {
  const form = useForm<LoginValues>({
    resolver: zodResolver(loginForm),
    defaultValues: { username, password: "" },
  });
  const [formError, setFormError] = useState<string>();
  const errors = form.formState.errors;

  const submit = form.handleSubmit(async (values) => {
    setFormError(undefined);
    try {
      await onSubmit(values);
    } catch (rejection) {
      const error = toApiError(rejection).error;
      if (!showFieldErrors(form.setError, error, { username: "username", password: "password" })) {
        setFormError(describeError(error));
      }
    }
  });

  return (
    <AuthCard title="Log in" intro={origin === undefined ? undefined : `Logging in to ${origin}`}>
      {notice === undefined ? null : (
        <Alert>
          <AlertDescription className="flex flex-col gap-2 text-foreground">
            <span>{notice.text}</span>
            {notice.reason === undefined ? null : (
              <span className="text-muted-foreground">{notice.reason}</span>
            )}
            {notice.logout === true ? (
              <Button
                type="button"
                size="sm"
                variant="outline"
                className="self-start"
                onClick={onLogOut}
              >
                Log out
              </Button>
            ) : null}
          </AlertDescription>
        </Alert>
      )}
      <form className="flex flex-col gap-4" noValidate onSubmit={(event) => void submit(event)}>
        <FormField id="login-username" label="Username" error={errors.username?.message}>
          {(control) => (
            <Input {...control} autoComplete="username" {...form.register("username")} />
          )}
        </FormField>
        <FormField id="login-password" label="Password" error={errors.password?.message}>
          {(control) => (
            <PasswordInput
              {...control}
              toggleLabel="Show password"
              autoComplete="current-password"
              {...form.register("password")}
            />
          )}
        </FormField>
        <FormError message={formError} />
        <Button type="submit" disabled={form.formState.isSubmitting}>
          Log in
        </Button>
      </form>
      <div className="flex justify-between text-sm">
        <Link to="/register" className="underline underline-offset-4">
          Register with an invite
        </Link>
        <Link to="/setup" className="underline underline-offset-4">
          Server settings
        </Link>
      </div>
    </AuthCard>
  );
}

/** A login found another account's session still held. */
export function StillSignedIn({
  username,
  onContinue,
  onLogOut,
}: {
  username: string;
  onContinue: () => void;
  onLogOut: () => void;
}) {
  return (
    <AuthCard title="Log in">
      <p>{`You're still signed in as ${username}.`}</p>
      <div className="flex gap-2">
        <Button type="button" onClick={onContinue}>
          {`Continue as ${username}`}
        </Button>
        <Button type="button" variant="outline" onClick={onLogOut}>
          Log out
        </Button>
      </div>
    </AuthCard>
  );
}
