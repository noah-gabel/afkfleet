//! The handlers, one module per resource (Plan.md §4). A handler only parses
//! its input, authorizes, calls a service and maps the result: no business
//! logic and no SQL.
//!
//! - [`accounts`]: `POST /api/v1/accounts/link`, `GET
//!   /api/v1/accounts/link/{flow_id}` (P9.4), `POST
//!   /api/v1/accounts/offline` (P9.7), and `GET /api/v1/accounts`, `GET
//!   /api/v1/accounts/awaiting-approval`, `GET` and `DELETE
//!   /api/v1/accounts/{id}`, `POST /api/v1/accounts/{id}/approve`,
//!   `/refuse` and `/relink` (P9.6).
//! - [`agents`]: `POST /api/v1/agents/enrollment-tokens` (P10.3).
//! - [`audit`]: `GET /api/v1/audit` (P11.9).
//! - [`bots`]: `GET /api/v1/bots`, `GET` and `PATCH /api/v1/bots/{id}`,
//!   `PUT /api/v1/bots/{id}/mode`, and `POST /api/v1/bots/{id}/start`,
//!   `/stop`, `/restart`, `/reset` and `/resume` (P11.2).
//! - [`chat`]: `GET` and `POST /api/v1/bots/{id}/chat`, and `POST
//!   /api/v1/bots/{id}/command` (P11.3).
//! - [`events`]: `POST /api/v1/events/ticket` and `GET /api/v1/events`, the
//!   live events socket (P11.5).
//! - [`grants`]: `GET` and `POST /api/v1/accounts/{id}/grants`, `PUT` and
//!   `DELETE /api/v1/accounts/{id}/grants/{user}` (P9.6).
//! - [`auth`]: `POST /api/v1/auth/login`, `/auth/login/mfa`,
//!   `/auth/refresh`, `/auth/logout`, `/auth/step-up` and `/auth/register`
//!   (P7.6).
//! - [`health`]: `GET /health/live` and `GET /health/ready` (P6.7).
//! - [`invites`]: `POST` and `GET /api/v1/invites`, `DELETE
//!   /api/v1/invites/{id}` (P7.9).
//! - [`modes`]: `GET /api/v1/modes` (P11.4).
//! - [`me`]: `GET /api/v1/me`, `POST /api/v1/me/password`, `GET
//!   /api/v1/me/sessions` and `DELETE /api/v1/me/sessions/{id}` (P7.6).
//! - [`mfa`]: `POST /api/v1/me/mfa/enroll`, `/confirm` and `/reset` (P7.7).
//! - [`users`]: `GET /api/v1/users` and `PATCH /api/v1/users/{id}` (P7.9).
//!
//! The helpers here turn what the services hand back into response fields.

pub(super) mod accounts;
pub(super) mod agents;
pub(super) mod audit;
pub(super) mod auth;
pub(super) mod bots;
pub(super) mod chat;
pub(super) mod events;
pub(super) mod grants;
pub(super) mod health;
pub(super) mod invites;
pub(super) mod me;
pub(super) mod mfa;
pub(super) mod modes;
pub(super) mod users;

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use fleet_api_types::accounts::{
    AccountKind, AccountResponse, AccountStatus, GrantLevel as ApiGrantLevel, RefusalReason,
};
use fleet_api_types::auth::USERNAME_MAX_CHARS;
use fleet_api_types::chat::{ChatDirection, ChatFailure, ChatLine, ChatLineKind, ChatLineSender};
use fleet_api_types::modes::{
    ModeOwnership, ModeResponse, ModeStep, ModeVisibility as ApiModeVisibility,
};
use fleet_api_types::users::UserLabel;
use fleet_api_types::{BoundedText, FieldError, PageQuery, SafeInt, SecretText, Timestamp};
use fleet_core::authz::{
    Actor, GrantLevel, ModeVisibility, Permission, Role, authorize, effective_level,
};
use fleet_core::chat::{ChatMessage, ChatSender, Redacted};
use fleet_core::id::UserId;
use fleet_core::value::Username;
use fleet_proto::control::ChatFailedReason;
use secrecy::{ExposeSecret as _, SecretString};

use super::error::ApiError;
use super::extractors::AuthUser;
use crate::app::users::ActingUser;
use crate::ports::accounts::ListedAccount;
use crate::ports::chat::{ChatEntry, ListedChat};
use crate::ports::modes::ListedMode;
use crate::ports::page::{Page, PageLimit};
use crate::ports::passwords::HashError;

/// The first 422 message for a new password the strength check refused;
/// zxcvbn's warning and suggestions follow it.
const TOO_WEAK: &str = "is too weak";

/// A new password the password service refused, as the API answers it
/// (ADR-0016): a weak one is 422 on `field` with "is too weak", then one
/// entry per warning and per suggestion in zxcvbn's own words; a full queue
/// is `busy`; anything else is the server's fault.
fn refused_password(field: &str, error: HashError) -> ApiError {
    match error {
        HashError::Weak {
            warning,
            suggestions,
        } => {
            let hints = warning
                .iter()
                .map(|warning| FieldError::new(field, warning.text()))
                .chain(
                    suggestions
                        .iter()
                        .map(|suggestion| FieldError::new(field, suggestion.text())),
                );
            ApiError::Validation(
                core::iter::once(FieldError::new(field, TOO_WEAK))
                    .chain(hints)
                    .collect(),
            )
        }
        HashError::Busy => ApiError::busy(),
        other @ (HashError::Random(_) | HashError::Failed(_)) => ApiError::internal(other),
    }
}

/// Who makes a write: the authenticated user, their session and their
/// client's address.
fn acting(user: &AuthUser, ip: IpAddr) -> ActingUser {
    ActingUser {
        actor: user.actor(),
        session_id: user.session_id(),
        ip,
    }
}

/// A secret the service made, as a response field.
fn secret_text<const MAX: usize>(secret: &SecretString) -> SecretText<MAX> {
    SecretText::new(secret.expose_secret().to_owned())
}

/// A stored time as a response field. Stored times are within the years 0
/// to 9999, so one outside them is a bug.
fn timestamp(at: DateTime<Utc>) -> Result<Timestamp, ApiError> {
    Timestamp::try_from(at).map_err(ApiError::internal)
}

/// A username as a response field: always within its cap.
fn username(name: &Username) -> BoundedText<USERNAME_MAX_CHARS> {
    BoundedText::new(name.as_str())
}

/// A user named next to something else: an invite's creator, an account's
/// owner, a grant's grantee or granter.
fn label(id: UserId, name: &Username) -> UserLabel {
    UserLabel {
        id,
        username: username(name),
    }
}

/// The page a list's query asks for. garde has checked the limit, so one
/// out of range is a bug.
fn page<I: Copy>(query: &PageQuery<I>) -> Result<Page<I>, ApiError> {
    Ok(Page {
        before: query.before,
        limit: PageLimit::new(query.limit_or_default()).map_err(ApiError::internal)?,
    })
}

/// An account as `viewer` sees it (ADR-0018, group E): its own facts and
/// the viewer's level for everyone, and who owns it and since when only
/// for Admins, the Owner and whoever holds Manage on it.
fn account_response(viewer: &Actor, account: &ListedAccount) -> Result<AccountResponse, ApiError> {
    let level = effective_level(viewer, account.owner, account.grant)
        .ok_or_else(|| ApiError::internal("an account the viewer may not see was answered"))?;
    let shows_owner = sees_owner(viewer, level);
    Ok(AccountResponse {
        id: account.id,
        name: BoundedText::new(account.name.as_str()),
        kind: AccountKind::from(account.kind).into(),
        status: AccountStatus::from(account.status).into(),
        reason: account
            .status
            .reason()
            .map(|reason| RefusalReason::from(reason).into()),
        level: ApiGrantLevel::from(level).into(),
        owner: shows_owner.then(|| label(account.owner.id, &account.owner_name)),
        linked_at: shows_owner
            .then(|| timestamp(account.created_at))
            .transpose()?,
    })
}

/// Whether `viewer`, whose level on an account is `level`, sees who owns it
/// (ADR-0018, group E) and who sent its bot's lines (ADR-0020, U3): Admins,
/// the Owner and whoever holds Manage on it.
fn sees_owner(viewer: &Actor, level: GrantLevel) -> bool {
    sees_owners_by_role(viewer) || level == GrantLevel::Manage
}

/// Whether `viewer` sees who owns anything by their role alone: Admins and
/// the Owner, who may list every user anyway. A mode has no grants, so this
/// is the whole of its owner rule (ADR-0020, S14).
fn sees_owners_by_role(viewer: &Actor) -> bool {
    viewer.role >= Role::Admin
}

/// Why the agent didn't send a user's line, as one of the four codes
/// `POST /bots/{id}/chat` answers a failure with (S5, S34): one match without
/// a wildcard, which the answer and the live `chat_failed` share, so a
/// reason the protocol adds can't go unanswered.
fn chat_failure(reason: ChatFailedReason) -> ChatFailure {
    match reason {
        ChatFailedReason::NotOnline
        | ChatFailedReason::Disconnected
        | ChatFailedReason::NotInWorld => ChatFailure::BotNotOnline,
        ChatFailedReason::ChatUnavailable => ChatFailure::ChatUnavailable,
        ChatFailedReason::RateLimited | ChatFailedReason::QueueFull => ChatFailure::RateLimited,
        ChatFailedReason::TimedOut
        | ChatFailedReason::SessionBusy
        | ChatFailedReason::AgentBusy
        | ChatFailedReason::UnknownBot
        | ChatFailedReason::ShuttingDown => ChatFailure::BotUnreachable,
    }
}

/// A line of a bot's chat as `viewer`, whose level on the bot's account is
/// `level`, may see it (ADR-0020), for the history and D's live events:
/// - **A command's arguments** (S6, S9) only for Manage holders: below
///   Manage a mode's or a user's command reads `/` and its name, with
///   `arguments_hidden`. Received lines are never cut.
/// - **Who sent a user's line** (U3) only for viewers who see the account's
///   owner ([`sees_owner`]), and only while that user exists; `mine` on the
///   viewer's own lines, which lifts neither rule.
/// - **The text** is cleaned on its way out, a line break becoming ` | `
///   (S11).
fn chat_line(viewer: &Actor, level: GrantLevel, line: &ListedChat) -> Result<ChatLine, ApiError> {
    let record = &line.record;
    let id = SafeInt::try_new(record.id.get()).map_err(ApiError::internal)?;
    let at = timestamp(record.at)?;
    Ok(match &record.entry {
        ChatEntry::Received(chat) => ChatLine {
            id,
            at,
            direction: ChatDirection::In.into(),
            kind: ChatLineKind::from(chat.kind()).into(),
            text: BoundedText::new(chat.text()),
            truncated: chat.is_truncated(),
            arguments_hidden: false,
            sender: chat.sender().map(line_sender),
            sent_by: None,
            mine: false,
        },
        ChatEntry::ModeSent(message) => sent_line(
            id,
            at,
            ChatLineKind::Mode,
            shown(message, level == GrantLevel::Manage),
            None,
            false,
        ),
        ChatEntry::UserSent { message, by } => {
            let sent_by = (*by)
                .zip(line.sent_by.as_ref())
                .filter(|_| sees_owner(viewer, level))
                .map(|(by, name)| label(by, name));
            let mine = *by == Some(viewer.id);
            sent_line(
                id,
                at,
                ChatLineKind::User,
                shown(message, level == GrantLevel::Manage),
                sent_by,
                mine,
            )
        }
    })
}

/// What a message a bot sends shows its viewer (ADR-0020): the whole
/// message when the viewer may see it `whole`, else without a command's
/// arguments. A chat line is whole for Manage on its bot's account (S6,
/// S9), a mode's chat step for whoever may edit the mode (S15).
fn shown(message: &ChatMessage, whole: bool) -> Redacted<'_> {
    if whole {
        Redacted {
            text: message.as_str(),
            arguments_hidden: false,
        }
    } else {
        message.without_arguments()
    }
}

/// A mode as `viewer` may see it (ADR-0020, S14, S15):
/// - **Whose it is** for everyone: built in, the viewer's, or someone
///   else's; who exactly owns it only for Admins and the Owner.
/// - **What the viewer may do**, each one `authorize()` call, so no client
///   needs a copy of the rules: change its name and steps, delete it, switch
///   its visibility.
/// - **A command's arguments** in a chat step only for a viewer who may
///   edit the mode; below that `/` and the name, with `arguments_hidden`.
///
/// The list checked `ViewMode` already, so a mode the viewer may not see is
/// a bug, answered as the internal error.
fn mode_response(viewer: &Actor, listed: &ListedMode) -> Result<ModeResponse, ApiError> {
    let context = listed.context();
    authorize(viewer, Permission::ViewMode, &context)
        .map_err(|_| ApiError::internal("a mode the viewer may not see was answered"))?;
    let may = |permission| authorize(viewer, permission, &context).is_ok();
    let mode = &listed.mode;
    let editable = may(Permission::UpdateMode {
        visibility: mode.visibility,
    });
    let switched = match mode.visibility {
        ModeVisibility::Private => ModeVisibility::Shared,
        ModeVisibility::Shared => ModeVisibility::Private,
    };
    let ownership = match &listed.owner {
        None => ModeOwnership::BuiltIn,
        Some(owner) if owner.user.id == viewer.id => ModeOwnership::Mine,
        Some(_) => ModeOwnership::SomeoneElse,
    };
    Ok(ModeResponse {
        id: mode.id,
        name: BoundedText::new(mode.name.as_str()),
        visibility: ApiModeVisibility::from(mode.visibility).into(),
        ownership: ownership.into(),
        owner: listed
            .owner
            .as_ref()
            .filter(|_| sees_owners_by_role(viewer))
            .map(|owner| label(owner.user.id, &owner.username)),
        editable,
        deletable: may(Permission::DeleteMode),
        visibility_editable: may(Permission::UpdateMode {
            visibility: switched,
        }),
        version: mode.version.get(),
        created_at: timestamp(mode.created_at)?,
        updated_at: timestamp(mode.updated_at)?,
        steps: mode
            .definition
            .steps()
            .iter()
            .map(|step| ModeStep::of(step, |message| shown(message, editable)))
            .collect(),
    })
}

/// A line a bot sent, of `kind`, as [`chat_line`] decided to show it.
fn sent_line(
    id: SafeInt,
    at: Timestamp,
    kind: ChatLineKind,
    shown: Redacted<'_>,
    sent_by: Option<UserLabel>,
    mine: bool,
) -> ChatLine {
    ChatLine {
        id,
        at,
        direction: ChatDirection::Out.into(),
        kind: kind.into(),
        text: BoundedText::new(shown.text),
        truncated: false,
        arguments_hidden: shown.arguments_hidden,
        sender: None,
        sent_by,
        mine,
    }
}

/// A received line's sender, as its Minecraft server claims it.
fn line_sender(sender: &ChatSender) -> ChatLineSender {
    ChatLineSender {
        name: BoundedText::new(sender.name()),
        uuid: sender
            .uuid()
            .map(|uuid| BoundedText::new(&uuid.hyphenated().to_string())),
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use fleet_api_types::bots::LastDisconnect;
    use fleet_core::chat::IncomingChat;
    use fleet_core::disconnect::DisconnectReason;
    use fleet_core::id::BotId;
    use proptest::prelude::*;

    use super::*;
    use crate::ports::chat::{ChatMessageId, ChatRecord};
    use fleet_core::authz::UserRef;

    /// `stored`, a received line, as the history lists it.
    fn received(stored: &IncomingChat) -> ListedChat {
        let at = DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
        ListedChat {
            record: ChatRecord {
                id: ChatMessageId::new(1),
                bot: BotId::new_v7(at, [1; 10]).unwrap(),
                at,
                entry: ChatEntry::Received(stored.clone()),
            },
            sent_by: None,
        }
    }

    fn viewer() -> Actor {
        Actor {
            id: UserId::new_v7(DateTime::UNIX_EPOCH, [2; 10]).unwrap(),
            role: Role::Member,
        }
    }

    proptest! {
        /// Every stored text crosses whole, each line break as ` | `, to
        /// every viewer (ADR-0020, S11): a stored text keeps up to 1024
        /// characters, so even one of only line breaks fits the 3072.
        #[test]
        fn a_stored_text_crosses_uncut_with_its_line_breaks_as_separators(
            raw in prop_oneof![
                "\n{1000,1100}",
                "[a-z\n]{900,1100}",
                "[a-z😀 \n]{0,1100}",
            ],
            level in prop_oneof![
                Just(GrantLevel::View),
                Just(GrantLevel::Control),
                Just(GrantLevel::Manage),
            ],
        ) {
            let stored = IncomingChat::system(&raw);

            let line = chat_line(&viewer(), level, &received(&stored)).unwrap();

            prop_assert_eq!(line.text.as_str(), stored.text().replace('\n', " | "));
            prop_assert!(!line.arguments_hidden);
        }

        /// Every kick message crosses whole, each line break as ` | `
        /// (ADR-0020, U7): it keeps up to 1024 characters, so even one of
        /// only line breaks fits the 3072.
        #[test]
        fn a_kick_message_crosses_uncut_with_its_line_breaks_as_separators(
            raw in prop_oneof![
                "\n{1000,1100}",
                "[a-z\n]{900,1100}",
                "[a-z😀 \n]{0,1100}",
            ],
        ) {
            let reason = DisconnectReason::kicked(Some("multiplayer.disconnect.kicked"), &raw);
            let kept = if let DisconnectReason::Kicked(kick) = &reason {
                kick.message().to_owned()
            } else {
                String::new()
            };

            let shown = LastDisconnect::from(&reason);

            prop_assert_eq!(
                shown.message.map(|message| message.as_str().to_owned()),
                Some(kept.replace('\n', " | "))
            );
        }
    }

    /// A user of `role`, numbered `n`.
    fn user_ref(n: u8, role: Role) -> UserRef {
        UserRef {
            id: UserId::new_v7(DateTime::UNIX_EPOCH, [n; 10]).unwrap(),
            role,
        }
    }

    /// A mode of `owner` (built in without one) with `visibility`, holding
    /// a command with arguments.
    fn listed_mode(owner: Option<UserRef>, visibility: ModeVisibility) -> ListedMode {
        let at = DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
        let definition = serde_json::from_value(serde_json::json!({ "steps": [
            { "action": { "type": "send_chat", "message": "/login hunter2" }, "schedule": { "type": "at_start" }, "probability": 100 }
        ] }))
        .unwrap();
        ListedMode {
            mode: crate::ports::modes::StoredMode {
                id: fleet_core::id::ModeId::new_v7(at, [9; 10]).unwrap(),
                owner: owner.map(|owner| owner.id),
                name: fleet_core::mode::ModeName::try_from("Mode").unwrap(),
                visibility,
                definition,
                version: core::num::NonZeroU32::MIN,
                created_at: at,
                updated_at: at,
            },
            owner: owner.map(|user| crate::ports::modes::ModeOwner {
                user,
                username: Username::try_from("someone").unwrap(),
            }),
        }
    }

    /// Every flag and field rule of a mode's answer, for every viewer role,
    /// every owner (built in, the viewer, a Member, an Admin, the Owner) and
    /// both visibilities: each right is exactly what `authorize()` answers,
    /// the owner shows only to Admins and the Owner, and a command's
    /// arguments only to a viewer who may edit the mode (S14, S15).
    #[test]
    fn each_flag_is_what_authorize_answers_and_each_field_follows_its_rule() {
        let mut seen = 0;
        for role in [Role::Member, Role::Admin, Role::Owner] {
            let viewer_ref = user_ref(1, role);
            let viewer = Actor {
                id: viewer_ref.id,
                role,
            };
            let owners = [
                None,
                Some(viewer_ref),
                Some(user_ref(2, Role::Member)),
                Some(user_ref(3, Role::Admin)),
                Some(user_ref(4, Role::Owner)),
            ];
            for owner in owners {
                for visibility in [ModeVisibility::Private, ModeVisibility::Shared] {
                    if owner.is_none() && visibility == ModeVisibility::Private {
                        continue;
                    }
                    let listed = listed_mode(owner, visibility);
                    let context = listed.context();
                    if authorize(&viewer, Permission::ViewMode, &context).is_err() {
                        assert!(mode_response(&viewer, &listed).is_err());
                        continue;
                    }
                    let may = |permission| authorize(&viewer, permission, &context).is_ok();
                    let other = match visibility {
                        ModeVisibility::Private => ModeVisibility::Shared,
                        ModeVisibility::Shared => ModeVisibility::Private,
                    };
                    let case = format!("{role:?} on {owner:?}'s {visibility:?}");

                    let answer = mode_response(&viewer, &listed).unwrap();

                    let editable = may(Permission::UpdateMode { visibility });
                    assert_eq!(answer.editable, editable, "{case}");
                    assert_eq!(answer.deletable, may(Permission::DeleteMode), "{case}");
                    assert_eq!(
                        answer.visibility_editable,
                        may(Permission::UpdateMode { visibility: other }),
                        "{case}"
                    );
                    assert_eq!(
                        answer.owner.is_some(),
                        owner.is_some() && role >= Role::Admin,
                        "{case}"
                    );
                    let ownership = match owner {
                        None => ModeOwnership::BuiltIn,
                        Some(owner) if owner.id == viewer.id => ModeOwnership::Mine,
                        Some(_) => ModeOwnership::SomeoneElse,
                    };
                    assert_eq!(answer.ownership.known(), Some(ownership), "{case}");
                    let action = &answer.steps.first().unwrap().action;
                    let message = action.message.as_ref().unwrap().as_str();
                    assert_eq!(
                        (message, action.arguments_hidden),
                        if editable {
                            ("/login hunter2", false)
                        } else {
                            ("/login", true)
                        },
                        "{case}"
                    );
                    seen += 1;
                }
            }
        }
        assert!(seen > 10, "the table reached only {seen} visible cases");
    }
}
