// Adding an offline account (Plan.md P9.7, P9.8, P11.11; S6 in ADR-0018's group F; ADR-0020, U107):
// for servers in offline mode, while the server allows them, on a screen of its own in the same card
// as the Microsoft link's. The app can't know that switch, so the screen is always there, and the
// server's 404 says it's off. Leaving it replaces its entry, so Back doesn't reopen it. A name is no
// secret, so this is a plain mutation.
import { zodResolver } from "@hookform/resolvers/zod";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
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
import { AuthCard } from "../../auth/components/AuthCard";
import {
  added,
  OFFLINE_CONFLICT,
  OFFLINE_HINT,
  OFFLINE_OFF,
  OFFLINE_TITLE,
  TILE_OFFLINE,
} from "../texts";

const offlineForm = z.object({ name: mcName });

type OfflineValues = z.infer<typeof offlineForm>;

/** `/accounts/offline`. */
export function OfflineScreen() {
  const api = useApiClient();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
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
      void navigate({ to: "/accounts", replace: true });
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
    <AuthCard title={OFFLINE_TITLE} intro={TILE_OFFLINE.line}>
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
        <div className="flex flex-wrap gap-2">
          <Button type="submit" disabled={create.isPending}>
            Add
          </Button>
          <Button asChild variant="outline">
            <Link to="/accounts" replace>
              Back to accounts
            </Link>
          </Button>
        </div>
      </form>
    </AuthCard>
  );
}
