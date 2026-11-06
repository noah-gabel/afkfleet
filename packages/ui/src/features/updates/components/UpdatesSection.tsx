// Profile's Updates section, beside Theme (Plan.md P12.5; ADR-0022, U27, U33): this PC's installed
// version, and a check on demand. A check the user ran says why it failed; the start's never
// does. An installed release's progress shows in the notice under the banner. The texts are the
// user's.
import { describeError } from "../../../api/describeError";
import { useUpdate } from "../../../app/context";
import { Button } from "../../../components/ui/button";
import { useUpdates } from "../hooks/useUpdates";

/** The section. */
export function UpdatesSection() {
  const current = useUpdate((state) => state.current);
  const check = useUpdate((state) => state.check);
  const checking = useUpdate((state) => state.checking);
  const failure = useUpdate((state) => state.checkFailure);
  const installing = useUpdate((state) => state.install === "installing");
  const updates = useUpdates();

  const off = check?.state === "off";
  const result = checking
    ? null
    : failure !== null
      ? `Couldn't check for updates: ${describeError(failure)}`
      : check?.state === "up_to_date"
        ? "afkfleet is up to date."
        : check?.state === "available"
          ? `afkfleet ${check.version} is available.`
          : off
            ? "Updates are off in this build."
            : null;
  return (
    <section
      aria-labelledby="updates-heading"
      className="flex flex-col gap-2 rounded-lg border bg-card p-3"
    >
      <h2
        id="updates-heading"
        className="text-xs font-semibold tracking-wider text-muted-foreground uppercase"
      >
        Updates
      </h2>
      {current === null ? null : <p className="text-sm">Installed: afkfleet {current}.</p>}
      {/* One line high even while it's empty, so nothing jumps while a check runs. */}
      <p role="status" className="min-h-5 text-sm text-muted-foreground">
        {result}
      </p>
      {off ? null : (
        <div className="flex flex-wrap gap-2">
          <Button
            type="button"
            size="sm"
            variant="outline"
            disabled={checking || installing}
            onClick={() => void updates.check(true)}
          >
            {checking ? "Checking…" : "Check for updates"}
          </Button>
          {check?.state === "available" && !checking && failure === null ? (
            <Button
              type="button"
              size="sm"
              disabled={installing}
              onClick={() => void updates.install()}
            >
              Install and restart
            </Button>
          ) : null}
        </div>
      )}
    </section>
  );
}
