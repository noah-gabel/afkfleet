// The account's warnings (Plan.md P8.7, ADR-0016), on the profile and on Home, where every
// sign-in lands, so they can't go unseen:
// - Wrong 2FA codes since the previous login (`failed_code_attempts`), the newest login's
//   own typos left out: a login step that never succeeded, which someone who knows the
//   password may have started.
// - 3 or fewer recovery codes left: the replacement issues 10 new ones.
// The profile links to the flow that helps; Home links to the profile, and each can be
// dismissed there for this sign-in (the notices store). They show as long as `me` says so.
import { Link } from "@tanstack/react-router";

import { useNotices, useSession } from "../../../app/context";
import type { HomeWarning } from "../../../app/stores";
import { NoticeLine } from "../../../components/NoticeLine";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import type { MeResponse } from "../../../generated/MeResponse";
import { formatTime } from "../../../lib/time";

/** At or below this many recovery codes, the app warns. */
export const RECOVERY_CODES_WARN_AT = 3;

/** The wrong-codes warning: `count` of them, the newest at `lastAt`. */
export function failedCodesWarning(count: number, lastAt: string | undefined): string {
  const when = lastAt === undefined ? "" : `, the last on ${formatTime(lastAt)}`;
  const what =
    count === 1
      ? "A wrong 2FA code was entered for your account since your previous login"
      : `${count} wrong 2FA codes were entered for your account since your previous login`;
  return `${what}${when}. If that wasn't you, someone may know your password: change it.`;
}

/** The recovery-codes warning, with `left` codes left. */
export function recoveryCodesWarning(left: number): string {
  const what =
    left === 0
      ? "You have no recovery codes left."
      : left === 1
        ? "You have only 1 recovery code left."
        : `You have only ${left} recovery codes left.`;
  return `${what} Replace your authenticator to get 10 new ones.`;
}

/** The warnings `user`'s account needs, on the profile or on Home. */
export function SecurityWarnings({ user, place }: { user: MeResponse; place: "home" | "profile" }) {
  const generation = useSession((session) => session.generation);
  const dismissed = useNotices((notices) => notices.dismissed);
  const dismiss = useNotices((notices) => notices.dismiss);
  const hidden = (warning: HomeWarning) =>
    place === "home" && dismissed.generation === generation && dismissed.warnings.includes(warning);
  const failed = user.failed_code_attempts;
  const showFailed = failed.count > 0 && !hidden("failedCodes");
  const showCodes = user.recovery_codes_left <= RECOVERY_CODES_WARN_AT && !hidden("recoveryCodes");
  if (!showFailed && !showCodes) {
    return null;
  }
  return (
    <div className="flex flex-col gap-2">
      {showFailed ? (
        <Warning
          text={failedCodesWarning(failed.count, failed.last_at)}
          place={place}
          action={{ to: "/profile/password", label: "Change password" }}
          onDismiss={() => dismiss("failedCodes", generation)}
        />
      ) : null}
      {showCodes ? (
        <Warning
          text={recoveryCodesWarning(user.recovery_codes_left)}
          place={place}
          action={{ to: "/profile/2fa", label: "Replace authenticator" }}
          onDismiss={() => dismiss("recoveryCodes", generation)}
        />
      ) : null}
    </div>
  );
}

function Warning({
  text,
  place,
  action,
  onDismiss,
}: {
  text: string;
  place: "home" | "profile";
  action: { to: "/profile/password" | "/profile/2fa"; label: string };
  onDismiss: () => void;
}) {
  if (place === "home") {
    // Home's compact line (U35): to the profile, and Dismiss.
    return (
      <NoticeLine
        action={
          <Button asChild size="xs" variant="outline">
            <Link to="/profile">Open your profile</Link>
          </Button>
        }
        onDismiss={onDismiss}
      >
        {text}
      </NoticeLine>
    );
  }
  return (
    <Alert>
      <AlertDescription className="flex flex-col gap-2 text-foreground">
        <p>{text}</p>
        <Button asChild size="sm" variant="outline" className="self-start">
          <Link to={action.to}>{action.label}</Link>
        </Button>
      </AlertDescription>
    </Alert>
  );
}
