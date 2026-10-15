// One account on the Accounts screen (Plan.md P9.8, the user's U12 at group F's plan review):
// a summary, the re-auth prompt where one is needed, and "Open" to the account's own screen.
// A link waiting for the user's approval gets Approve and Refuse on its card, in the waiting
// section.
import { Link } from "@tanstack/react-router";

import { Button } from "../../../components/ui/button";
import { Card, CardContent, CardHeader } from "../../../components/ui/card";
import type { AccountResponse } from "../../../generated/AccountResponse";
import { formatTime } from "../../../lib/time";
import type { AccountWrites } from "../hooks/useAccountWrites";
import { accountActions } from "../permissions";
import { accessLabel, kindLabel, linkedBy, statusLabel } from "../texts";
import { ReauthPrompt } from "./ReauthPrompt";
import { ReviewActions } from "./ReviewActions";

/** Who linked `account`, and when, if the viewer may see it; "you" for the viewer. */
export function linkedLine(account: AccountResponse, viewer: string | undefined): string | null {
  if (account.owner === undefined || account.linked_at === undefined) {
    return null;
  }
  const who = account.owner.id === viewer ? "you" : account.owner.username;
  return linkedBy(who, formatTime(account.linked_at));
}

/** `account`'s card; `review` adds Approve and Refuse. */
export function AccountCard({
  account,
  viewer,
  review,
}: {
  account: AccountResponse;
  viewer: string | undefined;
  review?: AccountWrites;
}) {
  const line = linkedLine(account, viewer);
  return (
    <Card className="gap-3 py-4">
      <CardHeader className="gap-1">
        <h3 className="font-semibold">{account.name}</h3>
        <p className="text-sm text-muted-foreground">
          {`${kindLabel(account.kind)} · ${statusLabel(account.status)} · ${accessLabel(account.level)}`}
        </p>
        {line === null ? null : <p className="text-sm text-muted-foreground">{line}</p>}
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <ReauthPrompt
          account={account}
          canLinkAgain={accountActions(account).linkAgain}
          place="card"
        />
        {review === undefined ? null : <ReviewActions account={account} writes={review} />}
        <Button asChild size="sm" variant="outline" className="self-start">
          <Link to="/accounts/$id" params={{ id: account.id }} aria-label={`Open ${account.name}`}>
            Open
          </Link>
        </Button>
      </CardContent>
    </Card>
  );
}
