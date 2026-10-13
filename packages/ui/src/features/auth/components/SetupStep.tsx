// The 2FA setup (Plan.md P8.7, ADR-0016): a new secret from `mfa_enroll`, shown as the
// server's QR code and as its key in groups of 4, then the app's first code to confirm it.
// The QR code is shown only through `<img src>`, and only when it's the SVG data URI the
// server sends: the CSP allows any `data:` image. "New QR code" asks for another secret
// (the old one is then void). The secret lives only in this step's state.
import { zodResolver } from "@hookform/resolvers/zod";
import { useCallback, useEffect, useRef, useState } from "react";
import { useForm } from "react-hook-form";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { FormField } from "../../../components/FormField";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import type { MfaEnrollResponse } from "../../../generated/MfaEnrollResponse";
import { showFieldErrors } from "../../../lib/formErrors";
import type { Step } from "../authFlowReducer";
import { endsIn, useDeadline } from "../hooks/useDeadline";
import { type CodeValues, codeForm, withoutSpaces } from "../schemas";
import { AuthCard, FormError } from "./AuthCard";

/** The only QR code the app shows: an SVG data URI. */
const QR_PREFIX = "data:image/svg+xml;base64,";

/** `secret` in groups of 4, for typing it in by hand. */
export function grouped(secret: string): string {
  return (secret.match(/.{1,4}/g) ?? []).join(" ");
}

/** The setup step. `onEnroll` and `onConfirm` throw what the step should show. */
export function SetupStep({
  step,
  onEnroll,
  onConfirm,
  onExpire,
  onCancel,
}: {
  step: Extract<Step, { kind: "setup" }>;
  onEnroll: () => Promise<MfaEnrollResponse>;
  onConfirm: (code: string) => Promise<void>;
  onExpire: () => void;
  onCancel: () => void;
}) {
  const lastMinute = useDeadline(step.deadline, onExpire);
  const [enrollment, setEnrollment] = useState<MfaEnrollResponse>();
  const [enrollError, setEnrollError] = useState<string>();
  const form = useForm<CodeValues>({
    resolver: zodResolver(codeForm),
    defaultValues: { code: "" },
  });
  const [formError, setFormError] = useState<string>();
  const errors = form.formState.errors;

  // A new secret at the start and on every "New QR code". Each request takes a ticket and
  // only the newest ticket's answer shows, since an older secret is void once a newer one
  // exists (and StrictMode starts the first request twice).
  const enroll = useRef(onEnroll);
  useEffect(() => {
    enroll.current = onEnroll;
  }, [onEnroll]);
  const newest = useRef(0);
  const requestSecret = useCallback(() => {
    newest.current += 1;
    const ticket = newest.current;
    enroll.current().then(
      (answer) => {
        if (ticket === newest.current) {
          setEnrollment(answer);
          setEnrollError(undefined);
        }
      },
      (rejection: unknown) => {
        if (ticket === newest.current) {
          setEnrollError(describeError(toApiError(rejection).error));
        }
      },
    );
  }, []);
  useEffect(() => {
    requestSecret();
    return () => {
      newest.current += 1;
    };
  }, [requestSecret]);

  const submit = form.handleSubmit(async (values) => {
    setFormError(undefined);
    if (Date.now() >= step.deadline) {
      onExpire();
      return;
    }
    try {
      await onConfirm(withoutSpaces(values.code));
    } catch (rejection) {
      const error = toApiError(rejection).error;
      if (!showFieldErrors(form.setError, error, { code: "code" })) {
        setFormError(describeError(error));
      }
    }
  });

  return (
    <AuthCard
      title="Set up two-factor authentication"
      intro="Scan the QR code with your authenticator app, or type the key into it, then enter the code it shows."
    >
      <p className="text-sm text-muted-foreground">{endsIn(step.expiresIn)}</p>
      {lastMinute ? (
        <p role="status" className="text-sm font-medium">
          This step ends in under a minute.
        </p>
      ) : null}
      <FormError message={enrollError} />
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
      <p className="text-sm text-muted-foreground">
        Only the newest QR code works. Delete any entry you scanned from an earlier one in your
        authenticator app.
      </p>
      <Button type="button" variant="outline" className="self-start" onClick={requestSecret}>
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
        <div className="flex gap-2">
          <Button type="submit" disabled={form.formState.isSubmitting}>
            Confirm
          </Button>
          <Button type="button" variant="outline" onClick={onCancel}>
            Cancel
          </Button>
        </div>
      </form>
    </AuthCard>
  );
}
