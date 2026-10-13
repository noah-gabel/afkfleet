// The server settings (Plan.md P8.7, ADR-0017), the same form for the first setup and an
// unreadable profile (`/setup`, signed out) and for a change while signed in (`/server`).
// - The address, and optionally a custom CA's certificates as PEM, pasted or read from a
//   file (checked for size before it's read; it only fills the field, so what's sent is
//   what's shown).
// - A current custom CA starts checked, with its certificates' fingerprints. Its PEM never
//   reaches the UI, so it must be given again to keep it; unchecking the box is the way to
//   the system's roots.
// - Signed in, the first Save warns that a different server or CA logs out.
// - A refused certificate is explained; a refused address or CA shows its rule next to it.
import { zodResolver } from "@hookform/resolvers/zod";
import { Link } from "@tanstack/react-router";
import { type ChangeEvent, useMemo, useRef, useState } from "react";
import { useForm } from "react-hook-form";

import { describeError } from "../../../api/describeError";
import { CA_PEM_MAX_BYTES } from "../../../api/limits";
import { useServerProfile } from "../../../api/queries";
import { useSession } from "../../../app/context";
import { FormField } from "../../../components/FormField";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import { Textarea } from "../../../components/ui/textarea";
import type { ServerProfileState } from "../../../generated/ServerProfileState";
import { showFieldErrors } from "../../../lib/formErrors";
import { displayOrigin } from "../../../lib/origin";
import { CERTIFICATE_HELP } from "../certificateHelp";
import { useSaveServer } from "../hooks/useSaveServer";
import { type ServerValues, serverForm } from "../schemas";
import { unreadableText } from "../unreadable";
import { CaDetails } from "./CaDetails";

/** A chosen file larger than a CA can be. */
export const FILE_TOO_LARGE = "The file is too large for a CA: at most 64 KiB.";

/** The server form, titled `title`. */
export function ServerForm({ title }: { title: "Server setup" | "Server" }) {
  const { data: state } = useServerProfile();
  if (state === undefined) {
    return (
      <main className="p-6">
        <p className="text-muted-foreground">Loading…</p>
      </main>
    );
  }
  return <ServerFormBody title={title} state={state} />;
}

function ServerFormBody({ title, state }: { title: string; state: ServerProfileState }) {
  const current = state.state === "set" ? state.profile : undefined;
  const hasCurrentCa = current?.custom_ca !== undefined;
  const signedIn = useSession((session) => session.phase === "signed_in");
  const schema = useMemo(() => serverForm(hasCurrentCa), [hasCurrentCa]);
  const form = useForm<ServerValues>({
    resolver: zodResolver(schema),
    defaultValues: { url: current?.url ?? "", useCa: hasCurrentCa, caPem: "" },
  });
  const { save, pending } = useSaveServer();
  const [confirming, setConfirming] = useState(false);
  const [formError, setFormError] = useState<string>();
  const fileInput = useRef<HTMLInputElement>(null);
  const useCa = form.watch("useCa");
  const errors = form.formState.errors;

  const submit = form.handleSubmit(async (values) => {
    setFormError(undefined);
    if (signedIn && !confirming) {
      setConfirming(true);
      return;
    }
    setConfirming(false);
    const error = await save(
      values.useCa ? { url: values.url, ca_pem: values.caPem } : { url: values.url },
    );
    if (error === undefined) {
      return;
    }
    switch (error.code) {
      case "certificate":
        setFormError(CERTIFICATE_HELP);
        return;
      case "unreachable":
      case "timed_out":
        setFormError(`${describeError(error)} Nothing changed.`);
        return;
      default:
        if (!showFieldErrors(form.setError, error, { url: "url", ca_pem: "caPem" })) {
          setFormError(describeError(error));
        }
    }
  });

  async function readFile(event: ChangeEvent<HTMLInputElement>): Promise<void> {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (file === undefined) {
      return;
    }
    if (file.size > CA_PEM_MAX_BYTES) {
      form.setError("caPem", { type: "file", message: FILE_TOO_LARGE });
      return;
    }
    form.setValue("caPem", await file.text(), { shouldValidate: true, shouldDirty: true });
  }

  return (
    <main className="flex flex-1 justify-center p-6">
      <div className="flex w-full max-w-lg flex-col gap-4">
        <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
        {state.state === "unreadable" ? (
          <Alert>
            <AlertDescription className="text-foreground">
              {unreadableText(state.reason)}
            </AlertDescription>
          </Alert>
        ) : null}
        <form className="flex flex-col gap-4" noValidate onSubmit={(event) => void submit(event)}>
          <FormField
            id="server-url"
            label="Server address"
            hint="The address the server's admin gave you, such as https://afk.example.com."
            error={errors.url?.message}
          >
            {(control) => (
              <Input {...control} autoComplete="url" spellCheck={false} {...form.register("url")} />
            )}
          </FormField>
          <label className="flex items-center gap-2 text-sm">
            <input type="checkbox" className="size-4 accent-primary" {...form.register("useCa")} />
            Use a custom CA
          </label>
          {useCa ? (
            <div className="flex flex-col gap-3">
              {current?.custom_ca === undefined ? null : <CaDetails ca={current.custom_ca} />}
              <FormField id="server-ca" label="CA certificates (PEM)" error={errors.caPem?.message}>
                {(control) => (
                  <Textarea
                    {...control}
                    rows={6}
                    spellCheck={false}
                    className="font-mono text-xs"
                    {...form.register("caPem")}
                  />
                )}
              </FormField>
              <input
                ref={fileInput}
                type="file"
                accept=".pem,.crt,.cer"
                aria-label="CA file"
                tabIndex={-1}
                className="sr-only"
                onChange={(event) => void readFile(event)}
              />
              <Button
                type="button"
                variant="outline"
                className="self-start"
                onClick={() => fileInput.current?.click()}
              >
                Choose file…
              </Button>
            </div>
          ) : null}
          {formError === undefined ? null : (
            <p role="alert" className="text-sm text-destructive">
              {formError}
            </p>
          )}
          {confirming && current !== undefined ? (
            <Alert>
              <AlertDescription className="flex flex-col gap-2 text-foreground">
                <span>{`Saving a different server or CA logs you out of ${displayOrigin(current.url)}.`}</span>
                <span className="flex gap-2">
                  <Button type="submit" size="sm" disabled={pending}>
                    Save and log out
                  </Button>
                  <Button
                    type="button"
                    size="sm"
                    variant="outline"
                    onClick={() => setConfirming(false)}
                  >
                    Cancel
                  </Button>
                </span>
              </AlertDescription>
            </Alert>
          ) : (
            <Button type="submit" className="self-start" disabled={pending}>
              Save
            </Button>
          )}
        </form>
        {!signedIn && state.state === "set" ? (
          <Link to="/login" className="text-sm underline underline-offset-4">
            Back to login
          </Link>
        ) : null}
      </div>
    </main>
  );
}
