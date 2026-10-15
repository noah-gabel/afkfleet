// The Accounts screen (Plan.md P9.8, the user's U7, U8 and U12 at group F's plan review):
// "Link Microsoft account", then, for Admins and the Owner, the links waiting for their
// approval as cards with Approve and Refuse, then every account the user may see as a card,
// a page at a time, and the offline accounts' form behind its own button.
import { Link } from "@tanstack/react-router";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { useAccounts, useMe } from "../../../api/queries";
import { Button } from "../../../components/ui/button";
import { useAccountWrites } from "../hooks/useAccountWrites";
import { useAwaitingApproval } from "../hooks/useAwaitingApproval";
import { AccountCard } from "./AccountCard";
import { OfflineSection } from "./OfflineForm";

/** The accounts the user may see. */
export function AccountsScreen() {
  const accounts = useAccounts();
  const { data: user } = useMe();
  const { reviewer, query: awaiting } = useAwaitingApproval();
  const writes = useAccountWrites();
  const items = accounts.data?.pages.flatMap((page) => page.items) ?? [];
  const waiting = reviewer ? (awaiting.data ?? []) : [];
  return (
    <main className="flex max-w-3xl flex-col gap-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <h1 className="text-2xl font-semibold tracking-tight">Accounts</h1>
        <Button asChild>
          <Link to="/accounts/link">Link Microsoft account</Link>
        </Button>
      </div>
      {waiting.length === 0 ? null : (
        <section aria-labelledby="waiting-heading" className="flex flex-col gap-3">
          <h2 id="waiting-heading" className="text-lg font-semibold">
            Waiting for your approval
          </h2>
          <ul aria-label="Waiting for your approval" className="flex flex-col gap-3">
            {waiting.map((account) => (
              <li key={account.id}>
                <AccountCard account={account} viewer={user?.id} review={writes} />
              </li>
            ))}
          </ul>
        </section>
      )}
      <section aria-labelledby={waiting.length === 0 ? undefined : "accounts-heading"}>
        {waiting.length === 0 ? null : (
          <h2 id="accounts-heading" className="mb-3 text-lg font-semibold">
            Accounts you can see
          </h2>
        )}
        {accounts.error === null ? null : (
          <p role="alert" className="text-sm text-destructive">
            {describeError(toApiError(accounts.error).error)}
          </p>
        )}
        {accounts.isSuccess && items.length === 0 ? (
          <p className="text-muted-foreground">
            No accounts yet. Link a Microsoft account to add one.
          </p>
        ) : (
          <ul aria-label="Accounts" className="flex flex-col gap-3">
            {items.map((account) => (
              <li key={account.id}>
                <AccountCard account={account} viewer={user?.id} />
              </li>
            ))}
          </ul>
        )}
        {accounts.hasNextPage ? (
          <Button
            type="button"
            variant="outline"
            className="mt-3 self-start"
            disabled={accounts.isFetchingNextPage}
            onClick={() => void accounts.fetchNextPage()}
          >
            Show more
          </Button>
        ) : null}
      </section>
      <OfflineSection />
    </main>
  );
}
