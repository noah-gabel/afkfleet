// The account screens' texts (Plan.md P9.8), as the user approved them at group F's plan
// review (ADR-0018). An open value this version doesn't know (a status, a reason, a flow's
// state) shows as the server sent it.
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
  "You'll get a code. Enter it on Microsoft's page and sign in with the Microsoft account that owns Minecraft: Java Edition.";

/** The link screen's first words, for linking `name` again. */
export function linkAgainIntro(name: string): string {
  return `Sign in with the Microsoft account that owns ${name}. Another account is refused.`;
}

/** The copy button's toasts. */
export const CODE_COPIED = "Code copied. Paste it on Microsoft's page.";
export const COPY_FAILED = "Couldn't copy. Select the code and copy it by hand.";

/** A status read that answers 404: the flow is gone for good. */
export const LINK_GONE = "This link ended or was replaced by a newer one.";

/** The start's refusals (S7): one text per status, each true for every cause. */
export const START_OVER_QUOTA =
  "You've reached your account limit. Delete an account to link another.";
export const START_PENDING = "This account is waiting for approval and can't be linked again yet.";
export const START_OFFLINE = "Offline accounts aren't linked through Microsoft.";
export const START_LIMITED =
  "You can't start a link right now. Your last link may still be finishing, or you've reached today's limit of new links.";
export const START_BUSY =
  "The server can't start a link right now: it's busy, or Microsoft can't be reached.";

/** A start that failed some other way. */
export function startFailed(detail: string): string {
  return `Couldn't get a code: ${detail}`;
}

/** The opener's failures, on the link screen. */
export const LINK_NOT_MICROSOFTS =
  "The server sent a sign-in page that isn't Microsoft's, so the app didn't show its code. Tell the server's admin.";
export const NOTHING_TO_OPEN = "There's no Microsoft page to open any more. Get a new code.";

/** Windows refused to open `link`. */
export function couldntOpen(link: string): string {
  return `Windows couldn't open Microsoft's page. Open ${link} in your browser yourself.`;
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
      return "Needs a new sign-in";
    case "refused":
      return "Refused";
    default:
      return status;
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
      return "This Microsoft account has no Minecraft profile yet. Start Minecraft once to create one, then link it again.";
    case "xbox_banned":
      return "Xbox has banned this Microsoft account.";
    case "xbox_no_xbox_profile":
      return "This Microsoft account has no Xbox profile yet. Sign in at xbox.com once, then link it again.";
    case "xbox_country_unavailable":
      return "Xbox Live isn't available in this Microsoft account's country.";
    case "xbox_adult_verification":
      return "Xbox needs this Microsoft account's age verified first.";
    case "xbox_child_account":
      return "This is a child's Microsoft account. An adult has to add it to a Microsoft family first.";
    case "xbox_other":
      return "Xbox refused this Microsoft account for another reason.";
    case "profile_mismatch":
      return `That sign-in belongs to another Minecraft profile than ${name}. Nothing changed.`;
    default:
      return `Refused (${reason}).`;
  }
}

/** The re-auth prompt's first sentence. */
export const NO_LONGER_ACCEPTED = "Microsoft no longer accepts this account's sign-in.";
/** What a viewer below Manage is asked to do. */
export const ASK_MANAGER = "Ask whoever manages it to link it again.";
/** The account screen's prompt for a Manage holder, after the first sentence. */
export const LINK_IT_AGAIN = "Link it again to keep using it.";
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
        ? "Waiting for you to enter the code on Microsoft's page."
        : `Waiting for you to enter the code on Microsoft's page. It works until ${details.until}.`;
    case "finishing":
      return "Microsoft confirmed your sign-in. Checking the Minecraft profile…";
    case "declined":
      return "The sign-in was declined at Microsoft.";
    case "expired":
      return "The code expired before it was entered.";
    case "taken":
      return "This Minecraft account is already linked in afkfleet.";
    case "over_quota":
      return START_OVER_QUOTA;
    case "refused":
      return details.reason === undefined
        ? "Refused."
        : reasonText(details.reason, details.name ?? "the account");
    case "failed":
      return "The link couldn't be finished.";
    case "linked":
      if (details.again) {
        return `${name} is linked again.`;
      }
      return details.accountStatus === "pending"
        ? `${name} is linked. It waits for an Admin's or the Owner's approval before a bot can use it.`
        : `${name} is linked.`;
    default:
      return `The server reports "${state}".`;
  }
}

/** Home's alert: how many links wait for the viewer's approval. */
export function waitingAlert(count: number): string {
  return count === 1
    ? "1 Microsoft link is waiting for your approval."
    : `${count} Microsoft links are waiting for your approval.`;
}

/** The nav badge's number. */
export function badgeNumber(count: number): string {
  return count > 99 ? "99+" : String(count);
}

/** A failed read of the waiting links, on Home. */
export function waitingFailed(detail: string): string {
  return `Couldn't check for links waiting for your approval: ${detail}`;
}

/** Who linked an account, and when. */
export function linkedBy(username: string, at: string): string {
  return `Linked by ${username} on ${at}`;
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
  return `${name} refused; its link was deleted.`;
}
export const NOT_WAITING = "This link isn't waiting for approval any more.";

/** The account screen's notes and answers. */
export const PENDING_NOTE =
  "This link waits for an Admin's or the Owner's approval. No bot can use it until then.";
export const NOT_FOUND = "This account doesn't exist, or you can't see it.";
export function deleteQuestion(name: string): string {
  return `Delete ${name}? Its stored sign-in and every grant go with it. You can link it again later.`;
}
export function deleted(name: string): string {
  return `${name} deleted.`;
}

/** The sharing section's texts. */
export const NOBODY_ELSE = "Nobody else has access.";
export const PENDING_NOT_SHARED = "A link waiting for approval can't be shared yet.";
export const TOO_MANY_NAMES = "You've tried too many names for now.";
export const ACCESS_HINTS: Readonly<Record<"view" | "control" | "manage", string>> = {
  view: "Sees its bot and chat.",
  control: "Also starts, stops and chats.",
  manage: "Everything you can do, sharing and deleting included.",
};
export function alreadyManages(user: string, name: string): string {
  return `${user} already manages ${name} through their role.`;
}
export function alreadyHasAccess(user: string): string {
  return `${user} already has access. Change their level in the list.`;
}
export function shared(name: string, user: string): string {
  return `${name} is shared with ${user}.`;
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

/** The offline form's texts (S6). */
export const OFFLINE_HINT =
  "For servers in offline mode. Anyone can join under this name there, and the connection isn't encrypted.";
export const OFFLINE_OFF = "This server doesn't allow offline accounts.";
export const OFFLINE_CONFLICT = "That name is taken, or you've reached your account limit.";
export function added(name: string): string {
  return `${name} added.`;
}
