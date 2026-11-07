// The update notice under the banner, on every screen (Plan.md P12.5; ADR-0022, U27, U28, U33):
// when the start's check, or one on Profile, found a newer version, until Later or the next start;
// then the install's progress, or why it failed. The texts are the user's. Its status region is
// always there, empty while there's nothing to say, so a screen reader hears it appear.
import { DownloadIcon } from "lucide-react";

import { describeError } from "../../../api/describeError";
import { useUpdate } from "../../../app/context";
import { Button } from "../../../components/ui/button";
import { useStartCheck, useUpdates } from "../hooks/useUpdates";

/** The text while an install runs. */
export const INSTALLING =
  "Downloading the update… afkfleet restarts by itself once it's installed.";

/** The notice; only its empty status region while there's nothing to say. */
export function UpdateNotice() {
  useStartCheck();
  const check = useUpdate((state) => state.check);
  const install = useUpdate((state) => state.install);
  const failure = useUpdate((state) => state.installFailure);
  const later = useUpdate((state) => state.later);
  const putAway = useUpdate((state) => state.putAway);
  const updates = useUpdates();

  const available = check?.state === "available" ? check.version : null;
  const text =
    install === "installing"
      ? INSTALLING
      : install === "failed" && failure !== null
        ? `The update wasn't installed: ${describeError(failure)}`
        : available !== null && !later
          ? `afkfleet ${available} is available.`
          : null;
  return (
    <div
      data-slot="update-notice"
      className={
        text === null
          ? undefined
          : // One height with or without buttons, so nothing jumps when an install starts.
            "flex min-h-11 items-center gap-3 border-b bg-[color-mix(in_srgb,var(--info)_10%,var(--background))] px-4 py-1.5 text-sm"
      }
    >
      {text === null ? null : <DownloadIcon className="size-4 shrink-0 text-info" aria-hidden />}
      <div role="status" className={text === null ? "sr-only" : "flex flex-1 flex-col gap-1"}>
        {text === null ? null : <p className="font-semibold">{text}</p>}
      </div>
      {text === null || install === "installing" ? null : (
        <div className="flex shrink-0 gap-2">
          {install === "failed" ? (
            <Button type="button" size="sm" onClick={() => void updates.tryAgain()}>
              Try again
            </Button>
          ) : (
            <Button type="button" size="sm" onClick={() => void updates.install()}>
              Install and restart
            </Button>
          )}
          <Button type="button" size="sm" variant="outline" onClick={putAway}>
            Later
          </Button>
        </div>
      )}
    </div>
  );
}
