// The start screen (Plan.md P8.6, ADR-0017): what `/` shows when the launch leads nowhere
// else. While the server can't be reached it waits (the banner retries). For any other
// error it shows the error and a way out: try again, the server settings, or log out, so
// a session the app can't use (a keychain entry it can't read) never leaves the user
// stuck.
import { useLoaderData, useNavigate, useRouter } from "@tanstack/react-router";

import { describeError } from "../../../api/describeError";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import type { CommandError } from "../../../generated/CommandError";
import { CERTIFICATE_HELP } from "../../server/certificateHelp";
import { useLogout } from "../hooks/useLogout";

/** The start screen. */
export function StartScreen() {
  const outcome = useLoaderData({ from: "/" });
  return (
    <main className="flex flex-1 items-center justify-center p-8">
      <div className="flex w-full max-w-md flex-col gap-4">
        <h1 className="text-3xl font-semibold tracking-tight">afkfleet</h1>
        {outcome.kind === "unreachable" ? (
          <p className="text-muted-foreground">Waiting for the server…</p>
        ) : (
          <Failed error={outcome.error} />
        )}
      </div>
    </main>
  );
}

function Failed({ error }: { error: CommandError }) {
  const router = useRouter();
  const navigate = useNavigate();
  const { logOut, pending } = useLogout();
  return (
    <>
      <Alert variant="destructive">
        <AlertDescription>{describeError(error)}</AlertDescription>
      </Alert>
      {error.code === "certificate" ? <p className="text-sm">{CERTIFICATE_HELP}</p> : null}
      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          onClick={() => {
            void router.invalidate();
          }}
        >
          Try again
        </Button>
        <Button
          type="button"
          variant="outline"
          onClick={() => {
            void navigate({ to: "/setup" });
          }}
        >
          Server settings
        </Button>
        <Button
          type="button"
          variant="outline"
          disabled={pending}
          onClick={() => {
            void logOut();
          }}
        >
          Log out
        </Button>
      </div>
    </>
  );
}
