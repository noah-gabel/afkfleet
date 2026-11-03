// The Accounts screen (Plan.md P9.8, P11.11; ADR-0020, U106, U108): both ways to add an account at
// the top, then, for Admins and the Owner, the links waiting for their approval as cards with Approve
// and Refuse, then every account the user may see as a card, a page at a time. With nothing to show,
// two choice tiles take the buttons' place, so nothing shows twice.
import { Link } from "@tanstack/react-router";
import { KeyRoundIcon, UserIcon } from "lucide-react";

import { toApiError } from "../../../api/ApiError";
import { describeError } from "../../../api/describeError";
import { useAccounts, useMe } from "../../../api/queries";
import { ChoiceTile } from "../../../components/ChoiceTile";
import { CARD_GRID } from "../../../components/linkedCard";
import { ScreenState } from "../../../components/ScreenState";
import { Button } from "../../../components/ui/button";
import { useAccountWrites } from "../hooks/useAccountWrites";
import { useAwaitingApproval } from "../hooks/useAwaitingApproval";
import {
  ADD_MICROSOFT,
  ADD_OFFLINE,
  LOADING_ACCOUNTS,
  NO_ACCOUNTS,
  TILE_MICROSOFT,
  TILE_OFFLINE,
} from "../texts";
import { AccountCard } from "./AccountCard";

/** The accounts the user may see. */
export function AccountsScreen() {
  const accounts = useAccounts();
  const { data: user } = useMe();
  const { reviewer, query: awaiting } = useAwaitingApproval();
  const writes = useAccountWrites();
  const items = accounts.data?.pages.flatMap((page) => page.items) ?? [];
  const waiting = reviewer ? (awaiting.data ?? []) : [];
  const empty = accounts.isSuccess && items.length === 0;
  const tiles = empty && waiting.length === 0;
  return (
    <main className="flex flex-1 flex-col gap-4 p-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h1 className="text-lg font-semibold tracking-tight">Accounts</h1>
        {accounts.isPending || tiles ? null : (
          <div className="flex flex-wrap gap-2">
            <Button asChild size="sm">
              <Link to="/accounts/link">
                <KeyRoundIcon aria-hidden="true" />
                {ADD_MICROSOFT}
              </Link>
            </Button>
            <Button asChild size="sm" variant="outline">
              <Link to="/accounts/offline">
                <UserIcon aria-hidden="true" />
                {ADD_OFFLINE}
              </Link>
            </Button>
          </div>
        )}
      </div>
      {waiting.length === 0 ? null : (
        <section aria-labelledby="waiting-heading" className="flex flex-col gap-2">
          <h2 id="waiting-heading" className="text-base font-semibold">
            Waiting for your approval
          </h2>
          <ul aria-label="Waiting for your approval" className={CARD_GRID}>
            {waiting.map((account) => (
              <li key={account.id}>
                <AccountCard account={account} viewer={user?.id} review={writes} />
              </li>
            ))}
          </ul>
        </section>
      )}
      {tiles ? (
        <div className="flex flex-1 flex-col items-center justify-center gap-3">
          <p className="text-sm text-muted-foreground">{NO_ACCOUNTS}</p>
          <div className="grid w-full max-w-2xl gap-3 @min-[48rem]/content:grid-cols-2">
            <ChoiceTile
              main
              to="/accounts/link"
              icon={KeyRoundIcon}
              title={TILE_MICROSOFT.title}
              line={TILE_MICROSOFT.line}
            />
            <ChoiceTile
              to="/accounts/offline"
              icon={UserIcon}
              title={TILE_OFFLINE.title}
              line={TILE_OFFLINE.line}
            />
          </div>
        </div>
      ) : (
        <section
          aria-labelledby={waiting.length === 0 ? undefined : "accounts-heading"}
          className="flex flex-col gap-2"
        >
          {waiting.length === 0 ? null : (
            <h2 id="accounts-heading" className="text-base font-semibold">
              Accounts you can see
            </h2>
          )}
          {accounts.error === null ? null : (
            <ScreenState kind="error">
              {describeError(toApiError(accounts.error).error)}
            </ScreenState>
          )}
          {accounts.isPending ? <ScreenState kind="loading">{LOADING_ACCOUNTS}</ScreenState> : null}
          {empty ? (
            <ScreenState kind="empty">{NO_ACCOUNTS}</ScreenState>
          ) : (
            <ul aria-label="Accounts" className={CARD_GRID}>
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
              className="self-start"
              disabled={accounts.isFetching}
              onClick={() => void accounts.fetchNextPage()}
            >
              Show more
            </Button>
          ) : null}
        </section>
      )}
    </main>
  );
}
