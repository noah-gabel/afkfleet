//! One cell's world (P13.1): serve's own app over a database of its own,
//! the same people and things in every one, and the cast who asks.
//!
//! - **The people:** `holder`, who owns the account `AfkBot1` and its bot,
//!   a Member or, in the matrix's second block, an Admin; `friend`, a
//!   Member; `steward`, an Admin; and the cast, a user of their own unless
//!   they're anonymous. Everyone logs in with [`PASSWORD`], hashed once for
//!   all worlds ([`Shared`]).
//! - **The things:** the holder's Microsoft account, active, its bot (no
//!   server, `afk`, stopped, placed nowhere), and one enrolled agent.
//! - **The config** is the default but for two settings a row needs:
//!   offline accounts are on, and `/spawn` is the allowlist.
//!
//! A row adds what it needs through the helpers here, then the cell's one
//! request goes through [`World::answer`]. Every value is invented.

use core::net::SocketAddr;
use core::time::Duration;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request};
use fleet_core::account::AccountStatus;
use fleet_core::auth::NewPassword;
use fleet_core::authz::{CommandAllowlist, GrantLevel, ModeVisibility, Role};
use fleet_core::bot::LastReport;
use fleet_core::chat::ChatFormats;
use fleet_core::disconnect::ConflictTexts;
use fleet_core::id::{AccountId, AgentId, BotId, ModeId, ServerSettingsId, SessionId, UserId};
use fleet_core::mode::{ModeDefinition, ModeName};
use fleet_core::system::Clock as _;
use fleet_core::value::{AgentName, ServerAddress, ServerKey, Username};
use fleet_server::cli::serve::ServerPorts;
use fleet_server::config::{AccountsConfig, Argon2Params, RateLimitConfig};
use fleet_server::grpc::reconciler::ReconcileTimings;
use fleet_server::infra::crypto::passwords::Argon2Passwords;
use fleet_server::metrics::Metrics;
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::agents::NewAgent;
use fleet_server::ports::bots::NewBot;
use fleet_server::ports::grants::AccountGrant;
use fleet_server::ports::modes::NewMode;
use fleet_server::ports::passwords::Passwords;
use fleet_server::ports::server_settings::NewServerSettings;
use fleet_server::ports::store::Store as _;
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{ExposeSecret as _, SecretBox, SecretString};
use serde_json::{Value, json};
use sqlx::sqlite::SqliteConnectOptions;
use tempfile::TempDir;
use tokio::io::DuplexStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use totp_rs::Totp;
use tower::ServiceExt as _;

use crate::support::http::{Person, Server, config};
use crate::support::link::{IP, USER_CODE};
use crate::support::msauth::{FakeMicrosoftAuth, Script, device_flow};
use crate::support::pace::{FakePace, PaceGate};
use crate::support::service::{T0, at, profile, setup_entry};

/// Everyone's password: strong, so the password service takes it.
pub(crate) const PASSWORD: &str = "correct horse battery staple";

/// Another strong password, for the rows that set one.
pub(crate) const NEW_PASSWORD: &str = "seven purple llamas juggling";

/// How long a cell waits for a socket's handshake or a database's close.
const PATIENCE: Duration = Duration::from_secs(10);

/// Who asks: the matrix's columns (Plan.md P13.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cast {
    /// Nobody: a request without a token.
    Anonymous,
    /// A Member with no grant on the holder's account.
    Member,
    /// A Member with a View grant on the holder's account.
    View,
    /// A Member with a Control grant on it.
    Control,
    /// A Member with a Manage grant on it.
    Manage,
    /// An Admin with no grant.
    Admin,
    /// The Owner, with no grant.
    Owner,
}

impl Cast {
    /// Every cast, in the columns' order.
    pub(crate) const ALL: [Self; 7] = [
        Self::Anonymous,
        Self::Member,
        Self::View,
        Self::Control,
        Self::Manage,
        Self::Admin,
        Self::Owner,
    ];

    /// The column's heading.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Anonymous => "anonymous",
            Self::Member => "Member",
            Self::View => "View",
            Self::Control => "Control",
            Self::Manage => "Manage",
            Self::Admin => "Admin",
            Self::Owner => "Owner",
        }
    }

    /// The cast's role, if they're a user.
    const fn role(self) -> Option<Role> {
        match self {
            Self::Anonymous => None,
            Self::Member | Self::View | Self::Control | Self::Manage => Some(Role::Member),
            Self::Admin => Some(Role::Admin),
            Self::Owner => Some(Role::Owner),
        }
    }

    /// The grant the cast holds on the holder's account, if any.
    pub(crate) const fn grant(self) -> Option<GrantLevel> {
        match self {
            Self::View => Some(GrantLevel::View),
            Self::Control => Some(GrantLevel::Control),
            Self::Manage => Some(GrantLevel::Manage),
            Self::Anonymous | Self::Member | Self::Admin | Self::Owner => None,
        }
    }
}

/// What every world shares: the folder their databases live in, the
/// password service, whose start hashes once, and [`PASSWORD`]'s hash.
pub(crate) struct Shared {
    dir: TempDir,
    passwords: Arc<Argon2Passwords>,
    hash: SecretString,
}

impl Shared {
    pub(crate) async fn new() -> Self {
        let passwords =
            Argon2Passwords::new(Argon2Params::default(), Arc::new(SeededRandom::new(7)))
                .await
                .unwrap();
        let hash = passwords
            .hash(
                &NewPassword::try_from(SecretString::from(PASSWORD)).unwrap(),
                &Username::try_from("holder").unwrap(),
            )
            .await
            .unwrap();
        Self {
            dir: TempDir::new().unwrap(),
            passwords: Arc::new(passwords),
            hash: SecretString::from(hash.expose_secret()),
        }
    }
}

/// What one request was answered: the status, and the error's code if the
/// body is the error envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cell {
    pub(crate) status: u16,
    pub(crate) code: String,
}

/// Whose token a request carries.
#[derive(Debug)]
pub(crate) enum Bearer {
    /// The cast's access token; none for the anonymous.
    Mine,
    /// This token instead, a setup token say; none for the anonymous, who
    /// can't have got one.
    Instead(Option<SecretString>),
}

/// Where a request goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Over {
    /// The API's router, in-process.
    Api,
    /// The metrics port's router, in-process.
    MetricsPort,
    /// The API's router over a served connection, as a WebSocket upgrade.
    Socket,
}

/// A cell's one request, but for its method, which is the row's.
#[derive(Debug)]
pub(crate) struct Call {
    pub(crate) path: String,
    pub(crate) body: Option<Value>,
    pub(crate) bearer: Bearer,
    pub(crate) over: Over,
}

impl Call {
    /// `path` on the API with the cast's token and no body.
    pub(crate) fn to(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            body: None,
            bearer: Bearer::Mine,
            over: Over::Api,
        }
    }

    /// The call with the JSON `body`.
    pub(crate) fn with(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }

    /// The call with `token` instead of the cast's access token.
    pub(crate) fn bearing(mut self, token: Option<SecretString>) -> Self {
        self.bearer = Bearer::Instead(token);
        self
    }

    /// The call over `over`.
    pub(crate) const fn over(mut self, over: Over) -> Self {
        self.over = over;
        self
    }
}

/// See the [module docs](self).
pub(crate) struct World {
    server: Server,
    clock: ManualClock,
    fake: Arc<FakeMicrosoftAuth>,
    pace: Arc<FakePace>,
    /// The link flows' pollers this world holds in their first wait.
    gates: Vec<PaceGate>,
    file: PathBuf,
    hash: SecretString,
    pub(crate) cast: Cast,
    holder_role: Role,
    /// The cast's user and session, unless they're anonymous.
    pub(crate) me: Option<Person>,
    pub(crate) holder: UserId,
    pub(crate) friend: UserId,
    pub(crate) steward: UserId,
    /// The holder's account, and its bot.
    pub(crate) account: AccountId,
    pub(crate) bot: BotId,
    pub(crate) agent: AgentId,
}

/// The ID numbers of the world's people and things.
mod n {
    pub(super) const HOLDER: u8 = 2;
    pub(super) const FRIEND: u8 = 3;
    pub(super) const STEWARD: u8 = 4;
    pub(super) const CAST: u8 = 9;
    pub(super) const ACCOUNT: u8 = 1;
    pub(super) const PENDING: u8 = 5;
    pub(super) const AGENT: u8 = 1;
}

impl World {
    /// A fresh world named `name` (its database's file) where `cast` asks
    /// and the holder has the role `holder_role`.
    pub(crate) async fn start(shared: &Shared, name: &str, cast: Cast, holder_role: Role) -> Self {
        let file = shared.dir.path().join(format!("{name}.db"));
        // An empty file is an empty database: the server's own connect
        // never creates one.
        std::fs::File::create_new(&file).unwrap();
        let options = SqliteConnectOptions::new().filename(&file);
        let clock = ManualClock::new(at(T0));
        let fake = Arc::new(FakeMicrosoftAuth::new());
        let pace = Arc::new(FakePace::new(Arc::new(clock.clone())));
        let ports = ServerPorts {
            clock: Arc::new(clock.clone()),
            limiter_clock: Arc::new(clock.clone()),
            random: Arc::new(SeededRandom::new(21)),
            microsoft: fake.clone(),
            pace: pace.clone(),
            reconcile: ReconcileTimings::default(),
            metrics: Metrics::prometheus(),
        };
        let mut config = config(
            &RateLimitConfig::default(),
            AccountsConfig {
                allow_offline: true,
                ..AccountsConfig::default()
            },
            None,
        );
        config.chat.command_allowlist = CommandAllowlist::try_new(&["/spawn"]).unwrap();
        let passwords: Arc<dyn Passwords> = shared.passwords.clone();
        let server = Server::start_from(
            options,
            &config,
            &SecretBox::new(Box::new([0x21; 32])),
            (ports, passwords),
            None,
        )
        .await;
        let mut world = Self {
            server,
            clock,
            fake,
            pace,
            gates: Vec::new(),
            file,
            hash: shared.hash.clone(),
            cast,
            holder_role,
            me: None,
            holder: user_id(n::HOLDER),
            friend: user_id(n::FRIEND),
            steward: user_id(n::STEWARD),
            account: AccountId::new_v7(at(T0), [n::ACCOUNT; 10]).unwrap(),
            bot: BotId::new_v7(at(T0), [n::ACCOUNT; 10]).unwrap(),
            agent: AgentId::new_v7(at(T0), [n::AGENT; 10]).unwrap(),
        };
        world.populate().await;
        if let Some(role) = cast.role() {
            world.me = Some(world.server.session_of(user_id(n::CAST), role).await);
        }
        world
    }

    /// The people and the things, in one transaction.
    async fn populate(&self) {
        let mut tx = self.server.db.write().await.unwrap();
        let mut people = vec![
            (self.holder, "holder", self.holder_role),
            (self.friend, "friend", Role::Member),
            (self.steward, "steward", Role::Admin),
        ];
        if let Some(role) = self.cast.role() {
            people.push((user_id(n::CAST), "cast", role));
        }
        for (id, name, role) in people {
            tx.users()
                .insert(&NewUser {
                    id,
                    username: Username::try_from(name).unwrap(),
                    password_hash: PasswordHash::new(self.hash.clone()).unwrap(),
                    role,
                    created_at: at(T0),
                })
                .await
                .unwrap();
        }
        tx.accounts()
            .insert(&microsoft_account(
                self.account,
                self.holder,
                n::ACCOUNT,
                AccountStatus::Active,
            ))
            .await
            .unwrap();
        tx.bots()
            .insert(&NewBot {
                id: self.bot,
                account: self.account,
                mode: fleet_core::mode::ModePreset::Afk.id(),
            })
            .await
            .unwrap();
        tx.agents()
            .insert(&NewAgent {
                id: self.agent,
                name: AgentName::try_from("agent-1").unwrap(),
                created_at: at(T0),
            })
            .await
            .unwrap();
        if let Some(level) = self.cast.grant() {
            tx.grants()
                .insert(&grant(self.account, user_id(n::CAST), level, self.holder))
                .await
                .unwrap();
        }
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Ends the world: its flows' pollers, its database and the file.
    pub(crate) async fn end(self) {
        if let Some(app) = &self.server.app {
            app.shutdown.cancel();
        }
        drop(self.gates);
        self.server
            .db
            .close_within(PATIENCE)
            .await
            .expect("the world's database closes");
        // The world's own file, in the matrix's own folder; a close removes
        // the WAL's files with it.
        remove_or_leave(&self.file);
    }

    // --- The people ----------------------------------------------------------------------

    /// The cast's user, or the holder for the anonymous, whose request is
    /// refused before it matters whose thing it names.
    pub(crate) fn me_or_holder(&self) -> UserId {
        self.me.as_ref().map_or(self.holder, |me| me.id)
    }

    /// The cast's access token, if they have one.
    fn my_token(&self) -> Option<&SecretString> {
        self.me.as_ref().map(|me| &me.access)
    }

    /// A new live session of `user`, who has `role`.
    pub(crate) async fn session_of(&self, user: UserId, role: Role) -> Person {
        self.server.session_of(user, role).await
    }

    /// A new live session of the holder.
    pub(crate) async fn holders_session(&self) -> Person {
        self.session_of(self.holder, self.holder_role).await
    }

    /// A second live session of the cast, or one of the holder's for the
    /// anonymous.
    pub(crate) async fn my_other_session(&self) -> SessionId {
        let (user, role) = self
            .me
            .as_ref()
            .map_or((self.holder, self.holder_role), |me| (me.id, me.role));
        self.session_of(user, role).await.session
    }

    /// Steps the cast's session up, through the route.
    pub(crate) async fn step_up(&self) {
        if let Some(token) = self.my_token() {
            let seen = self
                .server
                .request(
                    Method::POST,
                    "/api/v1/auth/step-up",
                    Some(token),
                    Some(json!({ "password": PASSWORD })),
                )
                .await;
            assert_eq!(seen.status, 204, "{}", seen.text);
        }
    }

    /// `username`'s first login step: the token it answers, a setup token
    /// before they enrolled and an mfa token after.
    pub(crate) async fn login(&self, username: &str) -> SecretString {
        let seen = self
            .server
            .request(
                Method::POST,
                "/api/v1/auth/login",
                None,
                Some(json!({ "username": username, "password": PASSWORD })),
            )
            .await;
        assert_eq!(seen.status, 200, "{}", seen.text);
        SecretString::from(seen.body["token"].as_str().unwrap())
    }

    /// Starts `username`'s enrollment with `setup`: their authenticator app.
    pub(crate) async fn enroll(&self, setup: &SecretString) -> Totp {
        let seen = self
            .server
            .request(Method::POST, "/api/v1/me/mfa/enroll", Some(setup), None)
            .await;
        assert_eq!(seen.status, 200, "{}", seen.text);
        Totp::from_url(seen.body["otpauth_uri"].as_str().unwrap()).unwrap()
    }

    /// The code `app` shows now.
    pub(crate) fn code(&self, app: &Totp) -> String {
        app.generate(u64::try_from(self.clock.now().timestamp()).unwrap())
            .to_string()
    }

    /// Sets up `username`'s 2FA through the login's own routes, and moves
    /// the clock on a step, so the app's next code is a new one.
    pub(crate) async fn enrolled(&self, username: &str) -> Totp {
        let setup = self.login(username).await;
        let app = self.enroll(&setup).await;
        let seen = self
            .server
            .request(
                Method::POST,
                "/api/v1/me/mfa/confirm",
                Some(&setup),
                Some(json!({ "code": self.code(&app) })),
            )
            .await;
        assert_eq!(seen.status, 200, "{}", seen.text);
        self.clock.advance(Duration::from_secs(30));
        app
    }

    /// Sets up the cast's 2FA, and gives them a session made after it.
    pub(crate) async fn enroll_me(&mut self) {
        if let Some(me) = &self.me {
            let (id, role) = (me.id, me.role);
            self.enrolled("cast").await;
            self.me = Some(self.session_of(id, role).await);
        }
    }

    /// A setup token from the cast's own login, if they can log in.
    pub(crate) async fn my_setup_token(&self) -> Option<SecretString> {
        match self.me {
            Some(_) => Some(self.login("cast").await),
            None => None,
        }
    }

    /// The code of a new invite for a Member, created by the steward
    /// through the route.
    pub(crate) async fn invite(&self) -> (String, SecretString) {
        let steward = self.session_of(self.steward, Role::Admin).await;
        let seen = self
            .server
            .request(
                Method::POST,
                "/api/v1/invites",
                Some(&steward.access),
                Some(json!({ "role": "member" })),
            )
            .await;
        assert_eq!(seen.status, 201, "{}", seen.text);
        (
            seen.body["invite"]["id"].as_str().unwrap().to_owned(),
            SecretString::from(seen.body["code"].as_str().unwrap()),
        )
    }

    // --- The things ----------------------------------------------------------------------

    /// Gives `user` `level` on the holder's account, as set by the holder.
    pub(crate) async fn share(&self, user: UserId, level: GrantLevel) {
        let mut tx = self.server.db.write().await.unwrap();
        tx.grants()
            .insert(&grant(self.account, user, level, self.holder))
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
    }

    /// A second account of the holder, a link that awaits its approval,
    /// with its bot; the cast holds on it what they hold on the first.
    pub(crate) async fn pending_account(&self) -> AccountId {
        let id = AccountId::new_v7(at(T0 + 1), [n::PENDING; 10]).unwrap();
        let mut tx = self.server.db.write().await.unwrap();
        tx.accounts()
            .insert(&microsoft_account(
                id,
                self.holder,
                n::PENDING,
                AccountStatus::Pending,
            ))
            .await
            .unwrap();
        tx.bots()
            .insert(&NewBot {
                id: BotId::new_v7(at(T0 + 1), [n::PENDING; 10]).unwrap(),
                account: id,
                mode: fleet_core::mode::ModePreset::Afk.id(),
            })
            .await
            .unwrap();
        if let (Some(level), Some(me)) = (self.cast.grant(), &self.me) {
            tx.grants()
                .insert(&grant(id, me.id, level, self.holder))
                .await
                .unwrap();
        }
        tx.commit(setup_entry()).await.unwrap();
        id
    }

    /// Stores `last` as the bot's last state, through an assignment to the
    /// agent that's then detached: the bot runs nowhere.
    pub(crate) async fn rested(&self, last: &LastReport) {
        let mut tx = self.server.db.write().await.unwrap();
        assert!(tx.bots().assign(self.bot, self.agent).await.unwrap());
        assert!(
            tx.bots()
                .record_state(self.bot, self.agent, last)
                .await
                .unwrap()
        );
        assert!(tx.bots().detach(self.bot, self.agent).await.unwrap());
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Disables the agent directly.
    pub(crate) async fn disable_agent(&self) {
        let mut tx = self.server.db.write().await.unwrap();
        assert!(tx.agents().disable(self.agent).await.unwrap());
        tx.commit(setup_entry()).await.unwrap();
    }

    /// A mode of `owner` with `visibility`, whose steps are `steps`,
    /// created a minute ago: an edit now is later than its creation.
    pub(crate) async fn mode(
        &self,
        owner: UserId,
        visibility: ModeVisibility,
        steps: &Value,
    ) -> ModeId {
        let created = at(T0 - 60_000);
        let id = ModeId::new_v7(created, [1; 10]).unwrap();
        let mut tx = self.server.db.write().await.unwrap();
        tx.modes()
            .insert(&NewMode {
                id,
                owner,
                name: ModeName::try_from("The holder's mode").unwrap(),
                visibility,
                definition: serde_json::from_value::<ModeDefinition>(json!({ "steps": steps }))
                    .unwrap(),
                created_at: created,
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        id
    }

    /// The settings of `mc.example.com`, with one conflict text.
    pub(crate) async fn server_settings(&self) -> ServerSettingsId {
        let id = ServerSettingsId::new_v7(at(T0), [1; 10]).unwrap();
        let mut tx = self.server.db.write().await.unwrap();
        tx.server_settings()
            .insert(&NewServerSettings {
                id,
                server: ServerKey::from(&"mc.example.com".parse::<ServerAddress>().unwrap()),
                conflict_texts: ConflictTexts::try_new(&["You logged in from another location"])
                    .unwrap(),
                chat_formats: ChatFormats::try_new(&[] as &[&str]).unwrap(),
                created_at: at(T0),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        id
    }

    /// Scripts Microsoft's answer to the next link flow's start, and holds
    /// that flow's poller in its first wait for the world's life.
    pub(crate) fn expect_link(&mut self) {
        self.gates.push(self.pace.hold_next());
        self.fake.script_start(Script::Answer(Ok(device_flow(
            "device-code",
            USER_CODE,
            at(T0 + 900_000),
        ))));
    }

    /// Starts a link flow as `by`, through the route: its ID.
    pub(crate) async fn link_flow(&mut self, by: &SecretString) -> String {
        self.expect_link();
        let seen = self
            .server
            .request(Method::POST, "/api/v1/accounts/link", Some(by), None)
            .await;
        assert_eq!(seen.status, 201, "{}", seen.text);
        seen.body["flow_id"].as_str().unwrap().to_owned()
    }

    /// A ticket for the cast's session, from its route, if they have one.
    pub(crate) async fn my_ticket(&self) -> Option<String> {
        let token = self.my_token()?;
        let seen = self
            .server
            .request(Method::POST, "/api/v1/events/ticket", Some(token), None)
            .await;
        assert_eq!(seen.status, 200, "{}", seen.text);
        Some(seen.body["ticket"].as_str().unwrap().to_owned())
    }

    // --- The request ---------------------------------------------------------------------

    /// What `method` to `call` is answered.
    pub(crate) async fn answer(&self, method: Method, call: Call) -> Cell {
        let bearer = match &call.bearer {
            Bearer::Mine => self.my_token(),
            Bearer::Instead(token) => token.as_ref(),
        };
        match call.over {
            Over::Api => send(&self.server.router, method, &call.path, bearer, call.body).await,
            Over::MetricsPort => {
                let router = self
                    .server
                    .app
                    .as_ref()
                    .expect("the world keeps its app")
                    .metrics
                    .clone()
                    .layer(MockConnectInfo(SocketAddr::new(IP, 4711)));
                send(&router, method, &call.path, bearer, call.body).await
            }
            Over::Socket => upgrade(&self.server.router, &call.path, bearer).await,
        }
    }
}

/// User `n`'s ID.
fn user_id(n: u8) -> UserId {
    UserId::new_v7(at(T0), [n; 10]).unwrap()
}

/// The Microsoft account `id` of `owner` in `status`, named `AfkBot<n>`.
fn microsoft_account(id: AccountId, owner: UserId, n: u8, status: AccountStatus) -> McAccount {
    McAccount {
        id,
        owner,
        name: format!("AfkBot{n}").parse().unwrap(),
        created_at: at(T0),
        credential: AccountCredential::Microsoft {
            uuid: profile(n).parse().unwrap(),
            refresh_envelope: vec![n; 61],
            status,
        },
    }
}

/// `user`'s grant of `level` on `account`, set by `by`.
fn grant(account: AccountId, user: UserId, level: GrantLevel, by: UserId) -> AccountGrant {
    AccountGrant {
        account,
        user,
        level,
        granted_by: by,
        granted_at: at(T0),
    }
}

/// Removes `file`, or leaves it to its folder's own removal when it can't
/// be removed now. On Windows something else may hold a file for a moment
/// after its last handle here closed (a scanner, the indexer): the removal
/// then fails with a sharing violation, which once failed the whole matrix
/// under the full suite's load. The file is in the matrix's own temporary
/// folder, which goes at the test's end; removing each world's file early
/// only keeps that folder small.
fn remove_or_leave(file: &Path) {
    let _ = std::fs::remove_file(file);
}

/// `method path` to `router` with `bearer` and the JSON `body`, if any.
async fn send(
    router: &Router,
    method: Method,
    path: &str,
    bearer: Option<&SecretString>,
    body: Option<Value>,
) -> Cell {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = bearer {
        request = request.header("authorization", format!("Bearer {}", token.expose_secret()));
    }
    let request = match body {
        Some(body) => request
            .header("content-type", "application/json")
            .body(Body::from(body.to_string())),
        None => request.body(Body::empty()),
    };
    let response = router.clone().oneshot(request.unwrap()).await.unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    Cell {
        status,
        code: code_of(&bytes),
    }
}

/// The error envelope's code in `body`, or nothing.
fn code_of(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|body| body["error"]["code"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// `router` served over one in-memory pipe, until it's dropped.
struct Piped {
    task: JoinHandle<()>,
}

/// Hands axum the one pipe.
struct PipeListener(mpsc::Receiver<DuplexStream>);

impl axum::serve::Listener for PipeListener {
    type Io = DuplexStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.0.recv().await {
            Some(pipe) => (pipe, SocketAddr::new(IP, 0)),
            // The cell is over: nothing connects again.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(SocketAddr::new(IP, 0))
    }
}

impl Drop for Piped {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// `GET path` to `router` as a WebSocket upgrade with `bearer`, over a
/// served in-memory pipe: 101 once the socket opened, or the refusal.
async fn upgrade(router: &Router, path: &str, bearer: Option<&SecretString>) -> Cell {
    let (connects, incoming) = mpsc::channel(1);
    let app = router.clone();
    let _served = Piped {
        task: tokio::spawn(async move {
            axum::serve(PipeListener(incoming), app.into_make_service())
                .await
                .unwrap();
        }),
    };
    let (client, server) = tokio::io::duplex(64 * 1024);
    connects.send(server).await.unwrap();
    let mut request = format!("ws://127.0.0.1{path}")
        .into_client_request()
        .unwrap();
    if let Some(token) = bearer {
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {}", token.expose_secret())).unwrap(),
        );
    }
    match timeout(PATIENCE, tokio_tungstenite::client_async(request, client))
        .await
        .expect("the handshake in time")
    {
        Ok((_socket, response)) => Cell {
            status: response.status().as_u16(),
            code: String::new(),
        },
        Err(WsError::Http(response)) => Cell {
            status: response.status().as_u16(),
            code: code_of(response.body().as_deref().unwrap_or_default()),
        },
        Err(other) => panic!("the handshake failed: {other}"),
    }
}

/// A file another handle holds without sharing it, as a scanner does on
/// Windows, is left where it is, and one nothing holds is removed.
#[cfg(windows)]
#[test]
fn a_file_something_still_holds_is_left_and_a_free_one_is_removed() {
    use std::os::windows::fs::OpenOptionsExt as _;

    let folder = TempDir::new().unwrap();
    let held = folder.path().join("held.db");
    let free = folder.path().join("free.db");
    std::fs::write(&held, b"held").unwrap();
    std::fs::write(&free, b"free").unwrap();
    // No sharing at all: any other open or delete is a sharing violation.
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&held)
        .unwrap();

    remove_or_leave(&held);
    remove_or_leave(&free);

    assert!(held.exists(), "the held file is left");
    assert!(!free.exists(), "the free file is removed");
    drop(holder);
}
