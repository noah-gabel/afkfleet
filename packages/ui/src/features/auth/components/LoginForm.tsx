// The password step (Plan.md P8.7): username and password, checked by the API's own
// schema before anything is sent. What the server refuses shows next to its field, or above
// the button with describeError's text; a 429's wait counts down, the button waiting with it
// (useRetryWait). A notice says why the flow
// came back here; one about a session the flow can't use offers "Log out", and one with a
// refusal's wait (a login that ran by itself) starts the countdown. While the
// flow's call runs (`pending`), its links and buttons wait.
import { zodResolver } from "@hookform/resolvers/zod";
import { Link } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { useForm } from "react-hook-form";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { FormField } from "../../../components/FormField";
import { PasswordInput } from "../../../components/PasswordInput";
import { RetryWaitMessage } from "../../../components/RetryWaitMessage";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import { useRetryWait } from "../../../hooks/useRetryWait";
import { showFieldErrors } from "../../../lib/formErrors";
import type { Notice } from "../authFlowReducer";
import { type LoginValues, loginForm } from "../schemas";
import { AuthCard, FormError } from "./AuthCard";

/** The login form. `onSubmit` throws what the form should show. */
export function LoginForm({
  origin,
  username,
  notice,
  pending,
  onSubmit,
  onLogOut,
}: {
  origin: string | undefined;
  username: string;
  notice: Notice | undefined;
  pending: boolean;
  onSubmit: (values: LoginValues) => Promise<void>;
  onLogOut: () => void;
}) {
  const form = useForm<LoginValues>({
    resolver: zodResolver(loginForm),
    defaultValues: { username, password: "" },
  });
  const [formError, setFormError] = useState<string>();
  const wait = useRetryWait();
  const errors = form.formState.errors;
  const noticeWait = notice?.wait;
  const { startFor } = wait;
  useEffect(() => {
    if (noticeWait !== undefined) {
      startFor(noticeWait);
    }
  }, [noticeWait, startFor]);

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
                disabled={pending}
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
        <RetryWaitMessage wait={wait} />
        <Button
          type="submit"
          disabled={pending || form.formState.isSubmitting || wait.waiting !== null}
        >
          Log in
        </Button>
      </form>
      <div className="flex justify-between text-sm">
        <Link to="/register" disabled={pending} className="underline underline-offset-4">
          Register with an invite
        </Link>
        <Link to="/setup" disabled={pending} className="underline underline-offset-4">
          Server settings
        </Link>
      </div>
    </AuthCard>
  );
}

/** A login found another account's session still held. */
export function StillSignedIn({
  username,
  pending,
  onContinue,
  onLogOut,
}: {
  username: string;
  pending: boolean;
  onContinue: () => void;
  onLogOut: () => void;
}) {
  return (
    <AuthCard title="Log in">
      <p>{`You're still signed in as ${username}.`}</p>
      <div className="flex gap-2">
        <Button type="button" disabled={pending} onClick={onContinue}>
          {`Continue as ${username}`}
        </Button>
        <Button type="button" variant="outline" disabled={pending} onClick={onLogOut}>
          Log out
        </Button>
      </div>
    </AuthCard>
  );
}
