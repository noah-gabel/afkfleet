// The account screens' mirror of `authorize()` (Plan.md P9.8, ADR-0010, ADR-0018): only what
// the server would allow is offered. Every account write but a review needs Manage, which the
// server reports as the viewer's own `level` (their grant or their implicit Manage, whichever
// is higher), so the screens decide from it. Who may review a pending link depends on its
// owner's role, which a viewer may not see: the screens offer Approve and Refuse only for the
// links `accountsAwaitingApproval` lists. The test reads fleet-core's authorization matrix
// itself, so a rule change on the server fails it until this file follows.
import type { AccountResponse } from "../../generated/AccountResponse";
import type { GrantLevel } from "../../generated/GrantLevel";

/** What a viewer may do with an account. */
export interface AccountActions {
  /** See and change who else has access, and share it. */
  readonly share: boolean;
  /** Link it again: a Microsoft account that isn't waiting for approval. */
  readonly linkAgain: boolean;
  /** Delete it. */
  readonly delete: boolean;
}

/** Whether `level` is Manage; an unknown level is not. */
export function manages(level: GrantLevel): boolean {
  return level === "manage";
}

/** The statuses an account may be linked again in; never a pending one (ADR-0018, U2). */
const LINKABLE_AGAIN: ReadonlySet<string> = new Set(["active", "reauth_required", "refused"]);

/** What the viewer whose own level is `account.level` may do with `account`. */
export function accountActions(
  account: Pick<AccountResponse, "kind" | "status" | "level">,
): AccountActions {
  const manage = manages(account.level);
  return {
    share: manage,
    linkAgain: manage && account.kind === "microsoft" && LINKABLE_AGAIN.has(account.status),
    delete: manage,
  };
}
