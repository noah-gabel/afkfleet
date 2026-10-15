// Answers the account screens' tests script their FakeApiClient with (Plan.md P9.8).
// Placeholders only (CLAUDE.md).
import type { AccountResponse } from "../generated/AccountResponse";
import type { GrantLevel } from "../generated/GrantLevel";
import type { GrantResponse } from "../generated/GrantResponse";
import type { LinkFlowResponse } from "../generated/LinkFlowResponse";
import type { LinkStartedResponse } from "../generated/LinkStartedResponse";
import { me } from "./fixtures";

/** The signed-in user's ID, as `me()` answers it. */
export const ME_ID = me().id;

/** Another user's ID. */
export const BOB_ID = "01900000-0000-7000-8000-000000000002";

/** Account `n`'s ID. */
export function accountId(n: number): string {
  return `01900000-0000-7000-8000-0000000001${String(n).padStart(2, "0")}`;
}

/** Account `n`, linked by the signed-in user, active, which they manage. */
export function account(n: number, changes: Partial<AccountResponse> = {}): AccountResponse {
  return {
    id: accountId(n),
    name: `AfkBot${n}`,
    kind: "microsoft",
    status: "active",
    level: "manage",
    owner: { id: ME_ID, username: "afkbot1" },
    linked_at: "2026-10-15T10:00:00.000Z",
    ...changes,
  };
}

/** Account `n` as a View or Control grantee sees it: no owner, no time. */
export function seenBelowManage(
  n: number,
  level: GrantLevel,
  changes: Partial<AccountResponse> = {},
) {
  const { owner: _owner, linked_at: _at, ...rest } = account(n, { level, ...changes });
  return rest;
}

/** Bob's grant of `level`, given by the signed-in user. */
export function grant(level: GrantLevel, user = { id: BOB_ID, username: "bob" }): GrantResponse {
  return {
    user,
    level,
    granted_by: { id: ME_ID, username: "afkbot1" },
    granted_at: "2026-10-15T11:00:00.000Z",
  };
}

/** The link flow's ID. */
export const FLOW = "01900000-0000-7000-8000-0000000002aa";

/** The user code of every start: a secret the caches must never hold. */
export const USER_CODE = "B7X4-KDLQ";

/** Microsoft's page. */
export const VERIFICATION_URI = "https://www.microsoft.com/link";

/** A start's answer. */
export const STARTED: LinkStartedResponse = {
  flow_id: FLOW,
  user_code: USER_CODE,
  verification_uri: VERIFICATION_URI,
  expires_at: "2026-10-15T10:15:00.000Z",
};

/** A flow's status answers. */
export const WAITING: LinkFlowResponse = {
  status: "waiting",
  expires_at: "2026-10-15T10:15:00.000Z",
};
export const FINISHING: LinkFlowResponse = { status: "finishing" };

/** A flow that ended `linked`, the account `n` in `accountStatus`. */
export function linked(
  n: number,
  accountStatus: "pending" | "active" = "pending",
): LinkFlowResponse {
  return {
    status: "linked",
    account_id: accountId(n),
    account_status: accountStatus,
    name: `AfkBot${n}`,
  };
}
