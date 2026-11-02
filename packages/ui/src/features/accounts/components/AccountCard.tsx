// One account on the Accounts screen (Plan.md P9.8, P11.11; ADR-0020, U106, R209): its name, status
// line and labelled facts, the whole card opening its screen, as a bot's on Home. The re-auth prompt
// where one is needed, and Approve and Refuse on a link waiting for the user, sit above the card's
// link, so they never open the account, their text can be selected and a step-up's field works.
import { Link } from "@tanstack/react-router";

import { type Fact, Facts } from "../../../components/Facts";
import { ABOVE_LINK, CARD_NAME, LINKED_CARD, STRETCHED_LINK } from "../../../components/linkedCard";
import { StatusLine } from "../../../components/StatusLine";
import { Card } from "../../../components/ui/card";
import type { AccountResponse } from "../../../generated/AccountResponse";
import type { AccountWrites } from "../hooks/useAccountWrites";
import { accountActions } from "../permissions";
import { ACCOUNT_LABEL, accessLabel, accountLine, kindLabel } from "../texts";
import { ReauthPrompt } from "./ReauthPrompt";
import { ReviewActions } from "./ReviewActions";

/** Who linked `account`, if the viewer may see it; "you" for the viewer. */
export function linkedByName(
  account: AccountResponse,
  viewer: string | undefined,
): string | undefined {
  return account.owner?.id === viewer ? "you" : account.owner?.username;
}

/** `account`'s facts on its card. */
function cardFacts(account: AccountResponse, viewer: string | undefined): Fact[] {
  const linked = linkedByName(account, viewer);
  return [
    { term: ACCOUNT_LABEL.type, value: kindLabel(account.kind) },
    { term: ACCOUNT_LABEL.access, value: accessLabel(account.level) },
    ...(linked === undefined ? [] : [{ term: ACCOUNT_LABEL.linkedBy, value: linked }]),
  ];
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
  const line = accountLine(account.status);
  return (
    <Card className={LINKED_CARD}>
      <h3 className={CARD_NAME}>
        <Link to="/accounts/$id" params={{ id: account.id }} className={STRETCHED_LINK}>
          {account.name}
        </Link>
      </h3>
      <StatusLine tone={line.tone}>{line.text}</StatusLine>
      <Facts facts={cardFacts(account, viewer)} place="card" />
      <div className={`${ABOVE_LINK} flex flex-col gap-2 empty:hidden`}>
        <ReauthPrompt
          account={account}
          canLinkAgain={accountActions(account).linkAgain}
          place="card"
        />
        {review === undefined ? null : <ReviewActions account={account} writes={review} />}
      </div>
    </Card>
  );
}
