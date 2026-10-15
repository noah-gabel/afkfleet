// Adding an offline account (Plan.md P9.7, P9.8, S6 in ADR-0018's group F): for servers in
// offline mode, while the server allows them. The app can't know that switch, so the form
// stays behind its own button, and the server's 404 says it's off. A name is no secret, so
// this is a plain mutation.
import { zodResolver } from "@hookform/resolvers/zod";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useForm } from "react-hook-form";
import { toast } from "sonner";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { describeError } from "../../../api/describeError";
import { ACCOUNTS_KEY } from "../../../api/queries";
import { mcName } from "../../../api/schemas";
import { z } from "../../../api/zod";
import { FormField } from "../../../components/FormField";
import { Button } from "../../../components/ui/button";
import { Input } from "../../../components/ui/input";
import { showFieldErrors } from "../../../lib/formErrors";
import { added, OFFLINE_CONFLICT, OFFLINE_HINT, OFFLINE_OFF } from "../texts";

const offlineForm = z.object({ name: mcName });

type OfflineValues = z.infer<typeof offlineForm>;

/** The button, and the form it opens. */
export function OfflineSection() {
  const [open, setOpen] = useState(false);
  if (!open) {
    return (
      <Button type="button" variant="outline" className="self-start" onClick={() => setOpen(true)}>
        Add an offline account
      </Button>
    );
  }
  return <OfflineForm onDone={() => setOpen(false)} />;
}

/** The form; `onDone` closes it. */
function OfflineForm({ onDone }: { onDone: () => void }) {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const form = useForm<OfflineValues>({
    resolver: zodResolver(offlineForm),
    defaultValues: { name: "" },
  });
  const [formError, setFormError] = useState<string>();
  const create = useMutation({
    mutationFn: (name: string) => api.createOfflineAccount({ name }),
    onSuccess: (account) => {
      toast.success(added(account.name));
      void queryClient.invalidateQueries({ queryKey: ACCOUNTS_KEY });
      onDone();
    },
    onError: (rejection) => {
      const error = toApiError(rejection).error;
      if (error.api?.status === 404) {
        setFormError(OFFLINE_OFF);
      } else if (error.api?.status === 409) {
        setFormError(OFFLINE_CONFLICT);
      } else if (!showFieldErrors(form.setError, error, { name: "name" })) {
        setFormError(describeError(error));
      }
    },
  });

  const submit = form.handleSubmit((values) => {
    setFormError(undefined);
    create.mutate(values.name);
  });

  return (
    <section aria-labelledby="offline-heading" className="flex flex-col gap-3">
      <h2 id="offline-heading" className="text-lg font-semibold">
        Offline account (test servers only)
      </h2>
      <form className="flex flex-col gap-4" noValidate onSubmit={(event) => void submit(event)}>
        <FormField
          id="offline-name"
          label="Minecraft name"
          hint={OFFLINE_HINT}
          error={form.formState.errors.name?.message}
        >
          {(control) => <Input {...control} autoComplete="off" {...form.register("name")} />}
        </FormField>
        {formError === undefined ? null : (
          <p role="alert" className="text-sm text-destructive">
            {formError}
          </p>
        )}
        <div className="flex gap-2">
          <Button type="submit" disabled={create.isPending}>
            Add
          </Button>
          <Button type="button" variant="outline" onClick={onDone}>
            Cancel
          </Button>
        </div>
      </form>
    </section>
  );
}
