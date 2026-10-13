// The banner above every screen (Plan.md P8.6, ADR-0017): up while the server can't be
// reached, is busy, or presents a certificate the app refuses. useServerRetry checks on a
// rhythm; a refused certificate is only checked on request, and leads to the server
// settings.
import { useNavigate } from "@tanstack/react-router";
import { ShieldAlertIcon, WifiOffIcon } from "lucide-react";

import { useConnection, useSession } from "../../../app/context";
import { Button } from "../../../components/ui/button";
import { CERTIFICATE_HELP } from "../../server/certificateHelp";
import { CHECK_EVERY_SECS, useServerRetry } from "../hooks/useServerRetry";

/** The banner; renders nothing while the connection is fine. */
export function ConnectionBanner() {
  const { status, retryAfterSecs, checking, retryNow } = useServerRetry();
  const detail = useConnection((state) => state.detail);
  const signedIn = useSession((state) => state.phase === "signed_in");
  const navigate = useNavigate();
  if (status === "ok") {
    return null;
  }

  const message =
    status === "certificate"
      ? CERTIFICATE_HELP
      : status === "busy"
        ? `The server is busy. Trying again in ${retryAfterSecs ?? CHECK_EVERY_SECS} s.`
        : `Can't reach the server. Trying again every ${CHECK_EVERY_SECS} s.`;
  return (
    <div role="status" className="border-b bg-muted px-4 py-2 text-sm">
      <div className="mx-auto flex max-w-5xl items-start gap-3">
        {status === "certificate" ? (
          <ShieldAlertIcon className="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden />
        ) : (
          <WifiOffIcon className="mt-0.5 size-4 shrink-0" aria-hidden />
        )}
        <div className="flex flex-1 flex-col gap-1">
          <p className="font-medium">{message}</p>
          {detail !== undefined && status !== "certificate" ? (
            <p className="text-muted-foreground">{detail}</p>
          ) : null}
        </div>
        <div className="flex shrink-0 gap-2">
          <Button type="button" size="sm" variant="outline" disabled={checking} onClick={retryNow}>
            Retry now
          </Button>
          {status === "certificate" ? (
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={() => {
                void navigate({ to: signedIn ? "/server" : "/setup" });
              }}
            >
              Server settings
            </Button>
          ) : null}
        </div>
      </div>
    </div>
  );
}
