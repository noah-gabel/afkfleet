// How many links wait for the user's approval (Plan.md P9.8, the user's U13 at group F's plan
// review): an alert on Home that can't be dismissed, one compact line above the bots since
// Phase 11's U35, and a badge on the nav's Accounts, for
// Admins and the Owner. Both are hidden while the count loads and when none wait; a read
// that failed says so on Home, and the badge then shows nothing (S5 in ADR-0018's group F).
import { Link } from "@tanstack/react-router";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { NoticeLine } from "../../../components/NoticeLine";
import { Button } from "../../../components/ui/button";
import { useAwaitingApproval } from "../hooks/useAwaitingApproval";
import { badgeNumber, waitingAlert, waitingFailed } from "../texts";

/** Home's alert. */
export function WaitingAlert() {
  const { reviewer, query } = useAwaitingApproval();
  if (!reviewer) {
    return null;
  }
  if (query.error !== null && query.data === undefined) {
    return (
      <p role="alert" className="text-sm text-destructive">
        {waitingFailed(describeError(toApiError(query.error).error))}
      </p>
    );
  }
  const count = query.data?.length ?? 0;
  if (count === 0) {
    return null;
  }
  return (
    <NoticeLine
      action={
        <Button asChild size="xs" variant="outline">
          <Link to="/accounts">Review</Link>
        </Button>
      }
    >
      {waitingAlert(count)}
    </NoticeLine>
  );
}

/**
 * How many links wait for the user's approval: 0 for a Member, while loading, or after a
 * failed read.
 */
export function useWaitingCount(): number {
  const { reviewer, query } = useAwaitingApproval();
  return reviewer ? (query.data?.length ?? 0) : 0;
}

/** The nav's name for Accounts with `count` links waiting; plain without any. */
export function accountsLinkName(count: number): string | undefined {
  return count === 0 ? undefined : `Accounts, ${count} waiting for your approval`;
}

/** The nav badge, inside the Accounts link: seen, not read out (the link's name says it). */
export function WaitingBadge({ count }: { count: number }) {
  if (count === 0) {
    return null;
  }
  return (
    <span
      aria-hidden="true"
      className="ml-2 rounded-full bg-primary px-1.5 py-0.5 text-xs font-medium text-primary-foreground"
    >
      {badgeNumber(count)}
    </span>
  );
}
