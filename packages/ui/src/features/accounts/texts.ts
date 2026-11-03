// The account screens' texts (Plan.md P9.8), as the user approved them at group F's plan
// review (ADR-0018). An open value this version doesn't know (a status, a reason, a flow's
// state) shows as the server sent it.
import type { Tone } from "../../components/StatusLine";
import type { AccountKind } from "../../generated/AccountKind";
import type { AccountStatus } from "../../generated/AccountStatus";
import type { GrantLevel } from "../../generated/GrantLevel";
import type { LinkFlowState } from "../../generated/LinkFlowState";
import type { RefusalReason } from "../../generated/RefusalReason";

/** The link screen's warning, in the user's own words (ADR-0018). */
export const LINK_WARNING =
  "Only enter a code you started yourself here. Never enter a code someone sent you, it would hand them your Minecraft account.";

/** The link screen's first words, for a new link. */
export const LINK_INTRO =
  "Get a code, enter it on Microsoft's page, and sign in with the account that owns Minecraft: Java Edition.";

/** The link screen's first words, for linking `name` again. */
export function linkAgainIntro(name: string): string {
  return `Sign in with the Microsoft account that owns ${name}. Another account is refused.`;
}

/** The copy button's toasts. */
export const CODE_COPIED = "Code copied. Paste it on Microsoft's page.";
export const COPY_FAILED = "Copy failed. Select the code by hand.";

/** A status read that answers 404: the flow is gone for good. */
export const LINK_GONE = "Link ended or replaced by a newer one.";

/** The start's refusals (S7): one text per status, each true for every cause. */
export const START_OVER_QUOTA = "Account limit reached. Delete one to link another.";
export const START_PENDING = "Waiting for approval. Can't be linked again yet.";
export const START_OFFLINE = "Offline accounts don't link through Microsoft.";
export const START_LIMITED =
  "Can't start a link now. Your last one may be finishing, or today's limit is reached.";
export const START_BUSY = "Can't start a link now. The server is busy or Microsoft is unreachable.";

/** A start that failed some other way. */
export function startFailed(detail: string): string {
  return `Couldn't get a code: ${detail}`;
}

/** The opener's failures, on the link screen. */
export const LINK_NOT_MICROSOFTS =
  "The server sent a sign-in page that isn't Microsoft's, so its code is hidden. Tell the server's admin.";
export const NOTHING_TO_OPEN = "No Microsoft page to open. Get a new code.";

/** Windows refused to open `link`. */
export function couldntOpen(link: string): string {
  return `Couldn't open Microsoft's page. Open ${link} in your browser.`;
}

/** How the app names an account's kind. */
export function kindLabel(kind: AccountKind): string {
  switch (kind) {
    case "microsoft":
      return "Microsoft";
    case "offline":
      return "Offline";
    default:
      return kind;
  }
}

/** How the app names an account's status. */
export function statusLabel(status: AccountStatus): string {
  switch (status) {
    case "pending":
      return "Waiting for approval";
    case "active":
      return "Active";
    case "reauth_required":
      return "Needs sign-in";
    case "refused":
      return "Refused";
    default:
      return status;
  }
}

/** An account's status as its status line shows it. */
export function accountLine(status: AccountStatus): { readonly text: string; readonly tone: Tone } {
  const text = statusLabel(status);
  switch (status) {
    case "active":
      return { text, tone: "success" };
    case "pending":
    case "reauth_required":
      return { text, tone: "warning" };
    case "refused":
      return { text, tone: "destructive" };
    default:
      return { text, tone: "neutral" };
  }
}

/** How the app names a level of access. */
export function accessLabel(level: GrantLevel): string {
  switch (level) {
    case "view":
      return "View";
    case "control":
      return "Control";
    case "manage":
      return "Manage";
    default:
      return level;
  }
}

/** Why Minecraft's side refused an account; `name` is the account's. */
export function reasonText(reason: RefusalReason, name: string): string {
  switch (reason) {
    case "not_owned":
      return "This Microsoft account doesn't own Minecraft: Java Edition.";
    case "no_profile":
      return "No Minecraft profile yet. Start Minecraft once, then link it again.";
    case "xbox_banned":
      return "Xbox banned this account.";
    case "xbox_no_xbox_profile":
      return "No Xbox profile yet. Sign in at xbox.com once, then link it again.";
    case "xbox_country_unavailable":
      return "Xbox Live isn't available in this account's country.";
    case "xbox_adult_verification":
      return "Xbox needs this account's age verified first.";
    case "xbox_child_account":
      return "A child's account. An adult must add it to a Microsoft family first.";
    case "xbox_other":
      return "Xbox refused this account.";
    case "profile_mismatch":
      return `Sign-in belongs to another profile than ${name}. Nothing changed.`;
    default:
      return `Refused (${reason}).`;
  }
}

/** The re-auth prompt's first sentence. */
export const NO_LONGER_ACCEPTED = "Microsoft no longer accepts its sign-in.";
/** What a viewer below Manage is asked to do. */
export const ASK_MANAGER = "Ask whoever manages it to link it again.";
/** The account screen's prompt for a Manage holder, after the first sentence. */
export const LINK_IT_AGAIN = "Link it again.";
/** The account screen's prompt for a refused account, after its reason. */
export const THEN_LINK_AGAIN = "Then link it again.";

/** A finished or running link flow's line; `name` is the account's when it's known. */
export function flowText(
  state: LinkFlowState,
  details: {
    readonly again: boolean;
    readonly name: string | undefined;
    readonly accountStatus: AccountStatus | undefined;
    readonly reason: RefusalReason | undefined;
    readonly until: string | undefined;
  },
): string {
  const name = details.name ?? "The account";
  switch (state) {
    case "waiting":
      return details.until === undefined
        ? "Enter the code on Microsoft's page."
        : `Enter the code on Microsoft's page. Works until ${details.until}.`;
    case "finishing":
      return "Sign-in confirmed. Checking the Minecraft profile…";
    case "declined":
      return "Sign-in declined at Microsoft.";
    case "expired":
      return "Code expired.";
    case "taken":
      return "This Minecraft account is already linked.";
    case "over_quota":
      return START_OVER_QUOTA;
    case "refused":
      return details.reason === undefined
        ? "Refused."
        : reasonText(details.reason, details.name ?? "the account");
    case "failed":
      return "Link failed.";
    case "linked":
      if (details.again) {
        return `${name} linked again.`;
      }
      return details.accountStatus === "pending"
        ? `${name} linked. Waits for an Admin's or the Owner's approval before a bot can use it.`
        : `${name} linked.`;
    default:
      return `The server reports "${state}".`;
  }
}

/** Home's alert: how many links wait for the viewer's approval. */
export function waitingAlert(count: number): string {
  return count === 1
    ? "1 Microsoft link waits for your approval."
    : `${count} Microsoft links wait for your approval.`;
}

/** The nav badge's number. */
export function badgeNumber(count: number): string {
  return count > 99 ? "99+" : String(count);
}

/** A failed read of the waiting links, on Home. */
export function waitingFailed(detail: string): string {
  return `Couldn't check waiting links: ${detail}`;
}

/** The review's questions and toasts. */
export function approveQuestion(name: string, owner: string): string {
  return `Approve ${name}, linked by ${owner}? Bots can use it from then on.`;
}
export function refuseQuestion(name: string, owner: string): string {
  return `Refuse ${name}? The link is deleted, and ${owner} can link it again.`;
}
export function approved(name: string): string {
  return `${name} approved.`;
}
export function refused(name: string): string {
  return `${name} refused. Link deleted.`;
}
export const NOT_WAITING = "No longer waiting for approval.";

/** The account screen's notes and answers. */
export const PENDING_NOTE =
  "Waits for an Admin's or the Owner's approval. No bot can use it until then.";
export const NOT_FOUND = "Account not found or not available to you.";
export function deleteQuestion(name: string): string {
  return `Delete ${name}? Its stored sign-in and every grant go with it. You can link it again later.`;
}
export function deleted(name: string): string {
  return `${name} deleted.`;
}
/** What asking before leaving says (ADR-0020, U23, S51). */
export function leaveQuestion(name: string): string {
  return `Leave ${name}? You lose access to it and its bot. Only someone who manages it can give it back.`;
}
export function left(name: string): string {
  return `You left ${name}.`;
}
/** A leave whose grant was revoked meanwhile. */
export function alreadyGone(name: string): string {
  return `No access to ${name} any more.`;
}

/** The sharing section's texts. */
export const NOBODY_ELSE = "Nobody else has access.";
/** The caller's own row: Leave is their way out, not Revoke (ADR-0020, U83). */
export const YOU = "You";
export const PENDING_NOT_SHARED = "Can't share a link waiting for approval.";
export const TOO_MANY_NAMES = "Too many names for now.";
export const ACCESS_HINTS: Readonly<Record<"view" | "control" | "manage", string>> = {
  view: "Sees its bot and chat.",
  control: "Also starts, stops and chats.",
  manage: "Everything, sharing and deleting included.",
};
export function alreadyManages(user: string, name: string): string {
  return `${user} already manages ${name} through their role.`;
}
export function alreadyHasAccess(user: string): string {
  return `${user} already has access. Change their level in the list.`;
}
export function shared(name: string, user: string): string {
  return `${name} shared with ${user}.`;
}
export function nowHas(user: string, level: GrantLevel): string {
  return `${user} now has ${accessLabel(level)}.`;
}
export function revokeQuestion(user: string, name: string): string {
  return `Revoke ${user}'s access to ${name}?`;
}
export function revoked(user: string): string {
  return `${user} no longer has access.`;
}

/** The labels of an account's facts. */
export const ACCOUNT_LABEL = {
  type: "Type",
  access: "Your access",
  linkedBy: "Linked by",
  linkedOn: "Linked on",
} as const;

/** The Accounts screen's ways to add an account (U106). */
export const ADD_MICROSOFT = "Link Microsoft account";
export const ADD_OFFLINE = "Add offline account";
export const NO_ACCOUNTS = "No accounts.";
export const LOADING_ACCOUNTS = "Loading accounts…";
export const LOADING_ACCOUNT = "Loading account…";
/** Who has access, while its list loads under an account already shown. */
export const LOADING_ACCESS = "Loading access…";
export const TILE_MICROSOFT = {
  title: "Microsoft account",
  line: "For online-mode servers. Signs in once with a code.",
} as const;
export const TILE_OFFLINE = {
  title: "Offline account",
  line: "For offline-mode servers. Just a name.",
} as const;
/** The offline screen's title and first words (U107). */
export const OFFLINE_TITLE = "Add an offline account";

/** The sharing form's field: its hint, and what its list of names is called (U110). */
export const SHARE_HINT = "Exact username, or pick a match.";
export const MATCHING_USERS = "Matching users";

/** The offline form's texts (S6). */
export const OFFLINE_HINT =
  "Offline-mode servers let anyone join under this name. The connection isn't encrypted.";
export const OFFLINE_OFF = "Offline accounts not allowed.";
export const OFFLINE_CONFLICT = "Name taken, or account limit reached.";
export function added(name: string): string {
  return `${name} added.`;
}
