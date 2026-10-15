// The re-auth prompt (Plan.md P9.8): an account that Microsoft no longer accepts
// (`reauth_required`), or that Minecraft's side refused (`refused`, with its reason), comes
// back only by linking it again. A Manage holder gets "Link again"; anyone else is asked to
// turn to whoever manages it. On a card the prompt is short; the account's screen says what
// to do (the user's choices U11 and U12 at group F's plan review).
import { Link } from "@tanstack/react-router";
import { Alert, AlertDescription } from "../../../components/ui/alert";
import { Button } from "../../../components/ui/button";
import type { AccountResponse } from "../../../generated/AccountResponse";
import {
  ASK_MANAGER,
  LINK_IT_AGAIN,
  NO_LONGER_ACCEPTED,
  reasonText,
  THEN_LINK_AGAIN,
} from "../texts";

/** The prompt for `account`, if it needs one; `canLinkAgain` is the viewer's right. */
export function ReauthPrompt({
  account,
  canLinkAgain,
  place,
}: {
  account: AccountResponse;
  canLinkAgain: boolean;
  place: "card" | "screen";
}) {
  if (account.status !== "reauth_required" && account.status !== "refused") {
    return null;
  }
  const lead =
    account.status === "refused"
      ? account.reason === undefined
        ? "Refused."
        : reasonText(account.reason, account.name)
      : NO_LONGER_ACCEPTED;
  const next = !canLinkAgain
    ? ASK_MANAGER
    : place === "card"
      ? undefined
      : account.status === "refused"
        ? THEN_LINK_AGAIN
        : LINK_IT_AGAIN;
  return (
    <Alert>
      <AlertDescription className="flex flex-col gap-2 text-foreground">
        <p>{next === undefined ? lead : `${lead} ${next}`}</p>
        {canLinkAgain ? (
          <Button asChild size="sm" variant="outline" className="self-start">
            <Link to="/accounts/$id/link-again" params={{ id: account.id }}>
              Link again
            </Link>
          </Button>
        ) : null}
      </AlertDescription>
    </Alert>
  );
}
