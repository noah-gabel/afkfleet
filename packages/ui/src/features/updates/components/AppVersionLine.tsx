// The app's version on a line of its own (Plan.md P12.5; ADR-0022, U37): at the bottom of the
// login screen and the start screen, so a friend who can't log in can tell their admin which
// version they have. The app answers it from itself, with no network call. The line keeps its
// height before the answer comes, so nothing moves when it does.
import { useAppVersion } from "../../../api/queries";

/** "afkfleet {version}", small and muted. */
export function AppVersionLine() {
  const version = useAppVersion();
  return (
    <p className="min-h-4 text-xs text-muted-foreground">
      {version.data === undefined ? null : `afkfleet ${version.data.version}`}
    </p>
  );
}
