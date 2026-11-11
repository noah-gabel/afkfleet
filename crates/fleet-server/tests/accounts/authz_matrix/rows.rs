//! The matrix's rows (P13.1): every route, once for each case worth
//! telling apart, and what each case puts into its world before the one
//! request.
//!
//! A row's request is always valid and aimed at a thing in the state the
//! route wants (a pending account to approve, a failed bot to reset), so
//! its answer is the authorization's and nothing else's. Where the world
//! can't give a caller who may act a success, the row's case says what
//! they get instead: nothing here runs a bot, so a restart and a chat line
//! end at their 409.

use axum::http::Method;
use fleet_core::authz::{GrantLevel, ModeVisibility, Role};
use fleet_core::bot::{FailReason, LastReport, LastState, PauseReason};
use fleet_core::disconnect::ConflictKind;
use fleet_core::id::ModeId;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};

use super::world::{Call, NEW_PASSWORD, Over, PASSWORD, World};
use crate::support::service::{T0, at};

/// One row: a route, the case it's asked in, and what that case arranges.
#[derive(Debug, Clone)]
pub(crate) struct Row {
    pub(crate) method: Method,
    /// The path's template, as the registry records it.
    pub(crate) path: &'static str,
    /// What sets this row apart from the route's other rows; empty when
    /// it's the only one.
    pub(crate) case: &'static str,
    pub(crate) scenario: Scenario,
}

/// What a row arranges, and the request it makes of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scenario {
    /// The path as it is, with the cast's token and no body.
    Plain,
    /// The path as it is, on the metrics port.
    MetricsPort,
    /// The live events socket: an upgrade with a ticket of the cast's
    /// session.
    Socket,
    /// The login, 2FA and what's the caller's own.
    Session(Session),
    /// Users and invites.
    People(People),
    /// Accounts, link flows and grants.
    Account(Account),
    /// Agents and bots.
    Fleet(Fleet),
    /// Modes and the servers' settings.
    Mode(Mode),
}

/// The rows of the login, 2FA and the caller's own things.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Session {
    /// The friend's password.
    Login,
    /// The friend's mfa token and their app's code.
    LoginMfa,
    /// A refresh token of the friend's.
    Refresh,
    /// The code of an invite the steward made.
    Register,
    /// The cast's own password.
    StepUp,
    /// The setup token of the cast's own login.
    Enroll,
    /// An access token, which a setup route refuses, and a code.
    ConfirmWithAccessToken,
    /// The setup token of the cast's own login, after its enroll.
    Confirm,
    /// The cast has 2FA to replace.
    MfaReset {
        stepped_up: bool,
    },
    Password {
        stepped_up: bool,
    },
    EndMyOther,
    EndTheFriends,
}

/// The rows of users and invites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum People {
    /// The friend, a Member, with this body.
    PatchFriend(&'static str),
    DisableSteward,
    /// The friend holds a grant to review.
    FriendsGrants,
    /// The holder shares their account with the friend.
    HoldersSharedAccounts,
    /// This body.
    Invite(&'static str),
    /// An invite the steward made, for a Member.
    RevokeInvite,
}

/// The rows of accounts, link flows and grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Account {
    StartLink,
    MyLinkFlow,
    HoldersLinkFlow,
    Offline,
    /// The holder's account, as it is.
    Holders,
    /// A link of the holder's that awaits its approval.
    Pending,
    Relink,
    GrantCandidates,
    ShareWithFriend,
    /// The friend holds View; this body changes it.
    ChangeFriendsGrant(&'static str),
    RevokeFriendsGrant,
}

/// The rows of agents and bots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fleet {
    Agent,
    /// The agent is disabled.
    ReleaseAgent,
    /// The holder's bot, as it is.
    Bot,
    /// The holder's bot, with this body.
    BotWith(&'static str),
    /// A shared mode that sends a command at start.
    BotMode {
        off_allowlist: bool,
    },
    ResetFailedBot,
    ResumePausedBot,
}

/// The rows of modes and the servers' settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Create { shared: bool, off_allowlist: bool },
    Update { of: Whose, off_allowlist: bool },
    Delete(Whose),
    CreateSettings,
    UpdateSettings,
    DeleteSettings,
    PreviewFormats,
}

/// Whose mode a row edits or deletes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Whose {
    /// The cast's own, private.
    Mine,
    /// The holder's, shared with everyone.
    HoldersShared,
    /// The holder's, private.
    HoldersPrivate,
}

impl From<Session> for Scenario {
    fn from(case: Session) -> Self {
        Self::Session(case)
    }
}

impl From<People> for Scenario {
    fn from(case: People) -> Self {
        Self::People(case)
    }
}

impl From<Account> for Scenario {
    fn from(case: Account) -> Self {
        Self::Account(case)
    }
}

impl From<Fleet> for Scenario {
    fn from(case: Fleet) -> Self {
        Self::Fleet(case)
    }
}

impl From<Mode> for Scenario {
    fn from(case: Mode) -> Self {
        Self::Mode(case)
    }
}

/// A command on the world's allowlist.
const ALLOWED: &str = "/spawn";

/// A command off it.
const OFF_LIST: &str = "/home base";

/// A step that swings the arm every 20 s.
fn swinging() -> Value {
    json!([{
        "action": { "type": "swing_arm" },
        "schedule": { "type": "every", "interval_ms": 20_000, "jitter_ms": 0 },
        "probability": 100,
    }])
}

/// A step that sends `command` at start.
fn sending(command: &str) -> Value {
    json!([{
        "action": { "type": "send_chat", "message": command },
        "schedule": { "type": "at_start" },
        "probability": 100,
    }])
}

/// A row's fixed body.
fn fixed(body: &str) -> Value {
    serde_json::from_str(body).unwrap()
}

/// `template` with its `{id}` replaced by `id`.
fn at_id(template: &str, id: &dyn core::fmt::Display) -> String {
    template.replace("{id}", &id.to_string())
}

/// Arranges `row`'s case in `world`, and says the request to make.
pub(crate) async fn arrange(row: &Row, world: &mut World) -> Call {
    let path = row.path;
    match row.scenario {
        Scenario::Plain => Call::to(path),
        Scenario::MetricsPort => Call::to(path).over(Over::MetricsPort),
        Scenario::Socket => {
            let ticket = world.my_ticket().await;
            let ticket = ticket.as_deref().unwrap_or("no-ticket");
            Call::to(format!("{path}?ticket={ticket}")).over(Over::Socket)
        }
        Scenario::Session(case) => session(case, path, world).await,
        Scenario::People(case) => people(case, path, world).await,
        Scenario::Account(case) => account(case, path, world).await,
        Scenario::Fleet(case) => fleet(case, path, world).await,
        Scenario::Mode(case) => mode(case, path, world).await,
    }
}

async fn session(case: Session, path: &str, world: &mut World) -> Call {
    match case {
        Session::Login => {
            Call::to(path).with(json!({ "username": "friend", "password": PASSWORD }))
        }
        Session::LoginMfa => {
            let app = world.enrolled("friend").await;
            let token = world.login("friend").await;
            Call::to(path).with(json!({
                "mfa_token": token.expose_secret(),
                "code": world.code(&app),
            }))
        }
        Session::Refresh => {
            let friend = world.session_of(world.friend, Role::Member).await;
            Call::to(path).with(json!({ "refresh_token": friend.refresh.expose_secret() }))
        }
        Session::Register => {
            let (_, code) = world.invite().await;
            Call::to(path).with(json!({
                "invite": code.expose_secret(),
                "username": "newcomer",
                "password": NEW_PASSWORD,
            }))
        }
        Session::StepUp => Call::to(path).with(json!({ "password": PASSWORD })),
        Session::Enroll => {
            let token = world.my_setup_token().await;
            Call::to(path).bearing(token)
        }
        Session::ConfirmWithAccessToken => Call::to(path).with(json!({ "code": "123456" })),
        Session::Confirm => match world.my_setup_token().await {
            Some(token) => {
                let app = world.enroll(&token).await;
                Call::to(path)
                    .with(json!({ "code": world.code(&app) }))
                    .bearing(Some(token))
            }
            None => Call::to(path)
                .with(json!({ "code": "123456" }))
                .bearing(None),
        },
        Session::MfaReset { stepped_up } => {
            world.enroll_me().await;
            if stepped_up {
                world.step_up().await;
            }
            Call::to(path)
        }
        Session::Password { stepped_up } => {
            if stepped_up {
                world.step_up().await;
            }
            Call::to(path).with(json!({ "new_password": NEW_PASSWORD }))
        }
        Session::EndMyOther => Call::to(at_id(path, &world.my_other_session().await)),
        Session::EndTheFriends => {
            let friend = world.session_of(world.friend, Role::Member).await;
            Call::to(at_id(path, &friend.session))
        }
    }
}

async fn people(case: People, path: &str, world: &World) -> Call {
    match case {
        People::PatchFriend(body) => Call::to(at_id(path, &world.friend)).with(fixed(body)),
        People::DisableSteward => {
            Call::to(at_id(path, &world.steward)).with(json!({ "disabled": true }))
        }
        People::FriendsGrants => {
            world.share(world.friend, GrantLevel::View).await;
            Call::to(at_id(path, &world.friend))
        }
        People::HoldersSharedAccounts => {
            world.share(world.friend, GrantLevel::View).await;
            Call::to(at_id(path, &world.holder))
        }
        People::Invite(body) => Call::to(path).with(fixed(body)),
        People::RevokeInvite => {
            let (invite, _) = world.invite().await;
            Call::to(at_id(path, &invite))
        }
    }
}

async fn account(case: Account, path: &str, world: &mut World) -> Call {
    let holders = at_id(path, &world.account);
    let friends_grant = holders.replace("{user}", &world.friend.to_string());
    match case {
        Account::StartLink => {
            world.expect_link();
            Call::to(path)
        }
        Account::MyLinkFlow => {
            let starter = match &world.me {
                Some(me) => me.access.clone(),
                None => world.holders_session().await.access,
            };
            let flow = world.link_flow(&starter).await;
            Call::to(path.replace("{flow_id}", &flow))
        }
        Account::HoldersLinkFlow => {
            let holder = world.holders_session().await;
            let flow = world.link_flow(&holder.access).await;
            Call::to(path.replace("{flow_id}", &flow))
        }
        Account::Offline => Call::to(path).with(json!({ "name": "AfkBot7" })),
        Account::Holders => Call::to(holders),
        Account::Pending => Call::to(at_id(path, &world.pending_account().await)),
        Account::Relink => {
            world.expect_link();
            Call::to(holders)
        }
        Account::GrantCandidates => Call::to(format!("{holders}?q=fri")),
        Account::ShareWithFriend => {
            Call::to(holders).with(json!({ "username": "friend", "level": "view" }))
        }
        Account::ChangeFriendsGrant(body) => {
            world.share(world.friend, GrantLevel::View).await;
            Call::to(friends_grant).with(fixed(body))
        }
        Account::RevokeFriendsGrant => {
            world.share(world.friend, GrantLevel::View).await;
            Call::to(friends_grant)
        }
    }
}

async fn fleet(case: Fleet, path: &str, world: &World) -> Call {
    let bot = at_id(path, &world.bot);
    match case {
        Fleet::Agent => Call::to(at_id(path, &world.agent)),
        Fleet::ReleaseAgent => {
            world.disable_agent().await;
            Call::to(at_id(path, &world.agent))
        }
        Fleet::Bot => Call::to(bot),
        Fleet::BotWith(body) => Call::to(bot).with(fixed(body)),
        Fleet::BotMode { off_allowlist } => {
            let command = if off_allowlist { OFF_LIST } else { ALLOWED };
            let mode = world
                .mode(world.holder, ModeVisibility::Shared, &sending(command))
                .await;
            Call::to(bot).with(json!({ "mode_id": mode.to_string() }))
        }
        Fleet::ResetFailedBot => {
            world
                .rested(&LastReport {
                    state: LastState::Failed(FailReason::Auth),
                    at: at(T0),
                })
                .await;
            Call::to(bot)
        }
        Fleet::ResumePausedBot => {
            world
                .rested(&LastReport {
                    state: LastState::Paused(PauseReason::Conflict {
                        kind: ConflictKind::DuplicateLogin,
                    }),
                    at: at(T0),
                })
                .await;
            Call::to(bot)
        }
    }
}

/// The mode a [`Whose`] names, made in `world`.
async fn mode_of(world: &World, whose: Whose) -> ModeId {
    let (owner, visibility) = match whose {
        Whose::Mine => (world.me_or_holder(), ModeVisibility::Private),
        Whose::HoldersShared => (world.holder, ModeVisibility::Shared),
        Whose::HoldersPrivate => (world.holder, ModeVisibility::Private),
    };
    world.mode(owner, visibility, &swinging()).await
}

async fn mode(case: Mode, path: &str, world: &World) -> Call {
    let steps = |off_allowlist| {
        if off_allowlist {
            sending(OFF_LIST)
        } else {
            swinging()
        }
    };
    match case {
        Mode::Create {
            shared,
            off_allowlist,
        } => Call::to(path).with(json!({
            "name": "A new mode",
            "visibility": if shared { "shared" } else { "private" },
            "steps": steps(off_allowlist),
        })),
        Mode::Update { of, off_allowlist } => {
            let shared = of == Whose::HoldersShared;
            Call::to(at_id(path, &mode_of(world, of).await)).with(json!({
                "version": 1,
                "name": "Renamed",
                "visibility": if shared { "shared" } else { "private" },
                "steps": steps(off_allowlist),
            }))
        }
        Mode::Delete(of) => Call::to(at_id(path, &mode_of(world, of).await)),
        Mode::CreateSettings => Call::to(path).with(json!({
            "server": "play.example.com",
            "conflict_texts": ["You logged in from another location"],
            "chat_formats": [],
        })),
        Mode::UpdateSettings => Call::to(at_id(path, &world.server_settings().await)).with(json!({
            "version": 1,
            "server": "mc.example.com",
            "conflict_texts": ["Kicked: logged in elsewhere"],
            "chat_formats": [],
        })),
        Mode::DeleteSettings => Call::to(at_id(path, &world.server_settings().await)),
        Mode::PreviewFormats => Call::to(path).with(json!({
            "chat_formats": ["{sender} : "],
            "line": "Steve : hello",
        })),
    }
}

/// A table of rows, a line each: `METHOD "path" "case" => scenario;`.
macro_rules! table {
    ($($method:ident $path:literal $case:literal => $scenario:expr;)*) => {
        vec![$(Row {
            method: Method::$method,
            path: $path,
            case: $case,
            scenario: Scenario::from($scenario),
        }),*]
    };
}

/// Every row, in the table's order.
pub(crate) fn rows() -> Vec<Row> {
    [
        session_rows(),
        people_and_account_rows(),
        fleet_rows(),
        mode_rows(),
    ]
    .concat()
}

fn session_rows() -> Vec<Row> {
    use Scenario::Plain;
    use Session as S;
    table![
        GET "/health/live" "" => Plain;
        GET "/health/ready" "" => Plain;
        POST "/api/v1/auth/login" "a user's password" => S::Login;
        POST "/api/v1/auth/login/mfa" "then that user's code" => S::LoginMfa;
        POST "/api/v1/auth/refresh" "a user's refresh token" => S::Refresh;
        POST "/api/v1/auth/register" "an invite's code" => S::Register;
        POST "/api/v1/auth/logout" "" => Plain;
        POST "/api/v1/auth/step-up" "the caller's password" => S::StepUp;
        POST "/api/v1/me/mfa/enroll" "with the session's access token" => Plain;
        POST "/api/v1/me/mfa/enroll" "with the setup token of the caller's login" => S::Enroll;
        POST "/api/v1/me/mfa/confirm" "with the session's access token" => S::ConfirmWithAccessToken;
        POST "/api/v1/me/mfa/confirm" "with the setup token of the caller's login" => S::Confirm;
        POST "/api/v1/me/mfa/reset" "stepped up" => S::MfaReset { stepped_up: true };
        POST "/api/v1/me/mfa/reset" "not stepped up" => S::MfaReset { stepped_up: false };
        GET "/api/v1/me" "" => Plain;
        POST "/api/v1/me/password" "stepped up" => S::Password { stepped_up: true };
        POST "/api/v1/me/password" "not stepped up" => S::Password { stepped_up: false };
        GET "/api/v1/me/sessions" "" => Plain;
        DELETE "/api/v1/me/sessions/{id}" "the caller's other session" => S::EndMyOther;
        DELETE "/api/v1/me/sessions/{id}" "another user's session" => S::EndTheFriends;
    ]
}

fn people_and_account_rows() -> Vec<Row> {
    use Account as A;
    use People as P;
    use Scenario::Plain;
    table![
        GET "/api/v1/users" "" => Plain;
        PATCH "/api/v1/users/{id}" "disable a Member" => P::PatchFriend(r#"{ "disabled": true }"#);
        PATCH "/api/v1/users/{id}" "make a Member an Admin" => P::PatchFriend(r#"{ "role": "admin" }"#);
        PATCH "/api/v1/users/{id}" "disable an Admin" => P::DisableSteward;
        GET "/api/v1/users/{id}/grants" "a Member's" => P::FriendsGrants;
        GET "/api/v1/users/{id}/shared-accounts" "a Member's" => P::HoldersSharedAccounts;
        POST "/api/v1/invites" "for a Member" => P::Invite(r#"{ "role": "member" }"#);
        POST "/api/v1/invites" "for an Admin" => P::Invite(r#"{ "role": "admin" }"#);
        GET "/api/v1/invites" "" => Plain;
        DELETE "/api/v1/invites/{id}" "an Admin's, for a Member" => P::RevokeInvite;
        POST "/api/v1/accounts/link" "" => A::StartLink;
        GET "/api/v1/accounts/link/{flow_id}" "the caller's own flow" => A::MyLinkFlow;
        GET "/api/v1/accounts/link/{flow_id}" "another user's flow" => A::HoldersLinkFlow;
        POST "/api/v1/accounts/offline" "" => A::Offline;
        GET "/api/v1/accounts" "" => Plain;
        GET "/api/v1/accounts/awaiting-approval" "" => Plain;
        GET "/api/v1/accounts/{id}" "" => A::Holders;
        DELETE "/api/v1/accounts/{id}" "" => A::Holders;
        POST "/api/v1/accounts/{id}/approve" "a pending link" => A::Pending;
        POST "/api/v1/accounts/{id}/refuse" "a pending link" => A::Pending;
        POST "/api/v1/accounts/{id}/relink" "" => A::Relink;
        GET "/api/v1/accounts/{id}/grants" "" => A::Holders;
        GET "/api/v1/accounts/{id}/grant-candidates" "" => A::GrantCandidates;
        POST "/api/v1/accounts/{id}/grants" "View for a Member" => A::ShareWithFriend;
        PUT "/api/v1/accounts/{id}/grants/{user}" "a Member's View to Control" => A::ChangeFriendsGrant(r#"{ "level": "control" }"#);
        PUT "/api/v1/accounts/{id}/grants/{user}" "a Member's View to Manage" => A::ChangeFriendsGrant(r#"{ "level": "manage" }"#);
        DELETE "/api/v1/accounts/{id}/grants/{user}" "a Member's" => A::RevokeFriendsGrant;
        DELETE "/api/v1/accounts/{id}/grants/me" "" => A::Holders;
    ]
}

fn fleet_rows() -> Vec<Row> {
    use Fleet as F;
    use Scenario::{Plain, Socket};
    table![
        GET "/api/v1/agents" "" => Plain;
        POST "/api/v1/agents/{id}/disable" "" => F::Agent;
        POST "/api/v1/agents/{id}/release" "a disabled agent" => F::ReleaseAgent;
        GET "/api/v1/audit" "" => Plain;
        GET "/api/v1/bots" "" => Plain;
        GET "/api/v1/bots/{id}" "" => F::Bot;
        PATCH "/api/v1/bots/{id}" "" => F::BotWith(r#"{ "server_address": "mc.example.com" }"#);
        PUT "/api/v1/bots/{id}/mode" "a mode with an allowlisted command" => F::BotMode { off_allowlist: false };
        PUT "/api/v1/bots/{id}/mode" "a mode with a command off the allowlist" => F::BotMode { off_allowlist: true };
        POST "/api/v1/bots/{id}/start" "" => F::Bot;
        POST "/api/v1/bots/{id}/stop" "" => F::Bot;
        POST "/api/v1/bots/{id}/restart" "a bot that runs nowhere" => F::Bot;
        POST "/api/v1/bots/{id}/reset" "a failed bot" => F::ResetFailedBot;
        POST "/api/v1/bots/{id}/resume" "a paused bot" => F::ResumePausedBot;
        GET "/api/v1/bots/{id}/chat" "" => F::Bot;
        POST "/api/v1/bots/{id}/chat" "plain chat, a bot that isn't online" => F::BotWith(r#"{ "message": "hello" }"#);
        POST "/api/v1/bots/{id}/chat" "an allowlisted command, a bot that isn't online" => F::BotWith(r#"{ "message": "/spawn" }"#);
        POST "/api/v1/bots/{id}/command" "a command off the allowlist, a bot that isn't online" => F::BotWith(r#"{ "message": "/home base" }"#);
        POST "/api/v1/events/ticket" "" => Plain;
        GET "/api/v1/events" "an upgrade with the session's ticket" => Socket;
    ]
}

fn mode_rows() -> Vec<Row> {
    use Mode as M;
    use Scenario::{MetricsPort, Plain};
    use Whose::{HoldersPrivate, HoldersShared, Mine};
    table![
        GET "/api/v1/modes" "" => Plain;
        POST "/api/v1/modes" "a private mode" => M::Create { shared: false, off_allowlist: false };
        POST "/api/v1/modes" "a shared mode" => M::Create { shared: true, off_allowlist: false };
        POST "/api/v1/modes/with-commands" "a private mode with a command off the allowlist" => M::Create { shared: false, off_allowlist: true };
        POST "/api/v1/modes/with-commands" "a shared mode with a command off the allowlist" => M::Create { shared: true, off_allowlist: true };
        PUT "/api/v1/modes/{id}" "the caller's own" => M::Update { of: Mine, off_allowlist: false };
        PUT "/api/v1/modes/{id}" "a Member's shared mode" => M::Update { of: HoldersShared, off_allowlist: false };
        PUT "/api/v1/modes/{id}" "a Member's private mode" => M::Update { of: HoldersPrivate, off_allowlist: false };
        PUT "/api/v1/modes/{id}/with-commands" "the caller's own, a command off the allowlist" => M::Update { of: Mine, off_allowlist: true };
        PUT "/api/v1/modes/{id}/with-commands" "a Member's shared mode, a command off the allowlist" => M::Update { of: HoldersShared, off_allowlist: true };
        DELETE "/api/v1/modes/{id}" "the caller's own" => M::Delete(Mine);
        DELETE "/api/v1/modes/{id}" "a Member's shared mode" => M::Delete(HoldersShared);
        DELETE "/api/v1/modes/{id}" "a Member's private mode" => M::Delete(HoldersPrivate);
        GET "/api/v1/server-settings" "" => Plain;
        POST "/api/v1/server-settings" "" => M::CreateSettings;
        PUT "/api/v1/server-settings/{id}" "" => M::UpdateSettings;
        DELETE "/api/v1/server-settings/{id}" "" => M::DeleteSettings;
        POST "/api/v1/server-settings/chat-format-preview" "" => M::PreviewFormats;
        GET "/metrics" "on the metrics port" => MetricsPort;
        GET "/metrics" "on the API's port" => Plain;
    ]
}
