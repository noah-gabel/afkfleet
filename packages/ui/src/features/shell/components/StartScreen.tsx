// The start screen (Plan.md P8.6, ADR-0017): what `/` shows when the launch leads nowhere
// else. While the server can't be reached it waits (the banner retries) and offers the server
// settings and a logout on this PC, so a server that moved or is gone for good never leaves
// the user stuck (ADR-0022, S1, U7). For any other error it shows the error and a way out:
// try again, the server settings, or log out, so a session the app can't use (a keychain
// entry it can't read) never does either. Both logouts are `logout`, which removes the stored
// session before it tries the server and says so when the server wasn't told. While one runs,
// every other choice waits: its end goes to /login, which would pull the user off the server
// settings or a launch run again.
import { useLoaderData, useNavigate, useRouter } from "@tanstack/react-router";

import { describeError } from "../../../api/describeError";
import { Logo, Wordmark } from "../../../components/Brand";
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
        <div className="flex items-center gap-3">
          <Logo size={32} />
          <h1 className="text-2xl tracking-tight">
            <Wordmark />
          </h1>
        </div>
        {outcome.kind === "unreachable" ? <Waiting /> : <Failed error={outcome.error} />}
      </div>
    </main>
  );
}

function Waiting() {
  const { logOut, pending } = useLogout();
  return (
    <>
      <p className="text-muted-foreground">Waiting for the server…</p>
      <div className="flex flex-wrap gap-2">
        <ServerSettings disabled={pending} />
        <Button
          type="button"
          variant="outline"
          disabled={pending}
          onClick={() => {
            void logOut();
          }}
        >
          Log out on this PC
        </Button>
      </div>
    </>
  );
}

function Failed({ error }: { error: CommandError }) {
  const router = useRouter();
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
          disabled={pending}
          onClick={() => {
            void router.invalidate();
          }}
        >
          Try again
        </Button>
        <ServerSettings disabled={pending} />
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

/** "Server settings", which opens the server setup; off while a logout runs. */
function ServerSettings({ disabled }: { disabled: boolean }) {
  const navigate = useNavigate();
  return (
    <Button
      type="button"
      variant="outline"
      disabled={disabled}
      onClick={() => {
        void navigate({ to: "/setup" });
      }}
    >
      Server settings
    </Button>
  );
}
