// The 2FA setup (Plan.md P8.7, ADR-0016): the step's secret, shown as the server's QR code
// and as its key in groups of 4, then the app's first code to confirm it. The flow requests
// the secret once per step and on "New QR code" (the old one is then void); while a new
// one loads, the old one is hidden and the step waits. The QR code is shown only through
// `<img src>`, and only when it's the SVG data URI the server sends: the CSP allows any
// `data:` image. The secret lives only in the flow's state for this step. The profile's
// authenticator replacement shows the same step with its own texts.
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
import type { SetupState } from "../authFlowReducer";
import { endsIn, useDeadline } from "../hooks/useDeadline";
import { type CodeValues, codeForm, withoutSpaces } from "../schemas";
import { AuthCard, FormError } from "./AuthCard";

/** The only QR code the app shows: an SVG data URI. */
const QR_PREFIX = "data:image/svg+xml;base64,";

/** The setup step's own texts. */
export interface SetupTexts {
  readonly title: string;
  readonly intro: string;
  /** Under the QR code: which one works. */
  readonly hint: string;
}

/** The first login's setup. */
export const SIGN_IN_SETUP: SetupTexts = {
  title: "Set up two-factor authentication",
  intro:
    "Scan the QR code with your authenticator app, or type the key into it, then enter the code it shows.",
  hint: "Only the newest QR code works. Delete any entry you scanned from an earlier one in your authenticator app.",
};

/** `secret` in groups of 4, for typing it in by hand. */
export function grouped(secret: string): string {
  return (secret.match(/.{1,4}/g) ?? []).join(" ");
}

/** The setup step. `onConfirm` throws what the step should show. */
export function SetupStep({
  step,
  texts = SIGN_IN_SETUP,
  pending,
  onNewSecret,
  onConfirm,
  onExpire,
  onCancel,
}: {
  step: SetupState;
  texts?: SetupTexts;
  /** The flow's call (the confirm) is running. */
  pending: boolean;
  onNewSecret: () => void;
  onConfirm: (code: string) => Promise<void>;
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
  const enrollment = step.loading ? undefined : step.enrollment;

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
      await onConfirm(withoutSpaces(values.code));
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
    <AuthCard title={texts.title} intro={texts.intro}>
      <p className="text-sm text-muted-foreground">{endsIn(step.expiresIn)}</p>
      {lastMinute ? (
        <p role="status" className="text-sm font-medium">
          This step ends in under a minute.
        </p>
      ) : null}
      <FormError message={step.enrollError} />
      {step.loading ? <p className="text-sm text-muted-foreground">Loading a new key…</p> : null}
      {enrollment === undefined ? null : (
        <div className="flex flex-col items-center gap-3">
          {enrollment.qr_svg.startsWith(QR_PREFIX) ? (
            <img
              src={enrollment.qr_svg}
              alt="QR code for your authenticator app"
              className="size-48 bg-white p-2"
            />
          ) : (
            <p role="alert" className="text-sm text-destructive">
              The server's QR code can't be shown. Type the key below into your authenticator app
              instead.
            </p>
          )}
          <p className="text-sm">
            Key: <code className="font-mono select-all">{grouped(enrollment.secret)}</code>
          </p>
        </div>
      )}
      <p className="text-sm text-muted-foreground">{texts.hint}</p>
      <Button
        type="button"
        variant="outline"
        className="self-start"
        disabled={step.loading || pending}
        onClick={onNewSecret}
      >
        New QR code
      </Button>
      <form className="flex flex-col gap-4" noValidate onSubmit={(event) => void submit(event)}>
        <FormField
          id="setup-code"
          label="Code"
          hint="The 6-digit code your app shows."
          error={errors.code?.message}
        >
          {(control) => (
            <Input
              {...control}
              inputMode="numeric"
              autoComplete="one-time-code"
              {...form.register("code")}
            />
          )}
        </FormField>
        <FormError message={formError} />
        <RetryWaitMessage wait={wait} />
        <div className="flex gap-2">
          <Button
            type="submit"
            disabled={
              step.loading || pending || form.formState.isSubmitting || wait.waiting !== null
            }
          >
            Confirm
          </Button>
          <Button type="button" variant="outline" disabled={pending} onClick={onCancel}>
            Cancel
          </Button>
        </div>
      </form>
    </AuthCard>
  );
}
