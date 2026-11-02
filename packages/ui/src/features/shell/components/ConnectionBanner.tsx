// The banner above every screen (Plan.md P8.6, ADR-0017, ADR-0020, S102): up while the server can't
// be reached, is busy, or presents a certificate the app refuses. useServerRetry checks on a rhythm;
// a refused certificate is only checked on request, and leads to the server settings.
//
// Its status region is always there, empty while the connection is fine, so a screen reader that
// watches it hears the banner go up, and holds only text, so its buttons aren't read as news. Its
// tints are opaque: the banner is sticky, and a screen scrolls beneath it.
import { useNavigate } from "@tanstack/react-router";
import { cn } from "cn";
import { ShieldAlertIcon, WifiOffIcon } from "lucide-react";

import { useConnection, useSession } from "../../../app/context";
import { Button } from "../../../components/ui/button";
import { CERTIFICATE_HELP } from "../../server/certificateHelp";
import { CHECK_EVERY_SECS, useServerRetry } from "../hooks/useServerRetry";

/** The banner; only its empty status region while the connection is fine. */
export function ConnectionBanner() {
  const { status, retryAfterSecs, checking, retryNow } = useServerRetry();
  const detail = useConnection((state) => state.detail);
  const signedIn = useSession((state) => state.phase === "signed_in");
  const navigate = useNavigate();
  const up = status !== "ok";
  const certificate = status === "certificate";

  const message = certificate
    ? CERTIFICATE_HELP
    : status === "busy"
      ? `Server busy. Retrying in ${retryAfterSecs ?? CHECK_EVERY_SECS} s.`
      : `Server unreachable. Retrying every ${CHECK_EVERY_SECS} s.`;
  return (
    <div
      data-slot="connection-banner"
      className={cn(
        up && "flex items-start gap-3 border-b px-4 py-2 text-sm",
        up && certificate && "bg-[color-mix(in_srgb,var(--destructive)_10%,var(--background))]",
        up && !certificate && "bg-[color-mix(in_srgb,var(--warning)_10%,var(--background))]",
      )}
    >
      {!up ? null : certificate ? (
        <ShieldAlertIcon className="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden />
      ) : (
        <WifiOffIcon className="mt-0.5 size-4 shrink-0 text-warning" aria-hidden />
      )}
      <div role="status" className={up ? "flex flex-1 flex-col gap-1" : "sr-only"}>
        {up ? <p className="font-semibold">{message}</p> : null}
        {up && detail !== undefined && !certificate ? (
          <p className="text-muted-foreground">{detail}</p>
        ) : null}
      </div>
      {up ? (
        <div className="flex shrink-0 gap-2">
          <Button type="button" size="sm" variant="outline" disabled={checking} onClick={retryNow}>
            Retry now
          </Button>
          {certificate ? (
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
      ) : null}
    </div>
  );
}
