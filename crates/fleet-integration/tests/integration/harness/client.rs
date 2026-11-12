//! The Owner (ADR-0019, S72): fleet-client over the server's real HTTP
//! listener, logged in with 2FA as the app does it, never stepped up
//! (ADR-0028), and changing bots only through the API, so every change
//! hints the reconciler as a user's does. The server's clock is frozen
//! between the case's own moves, so the session never expires.

use core::time::Duration;

use fleet_api_types::accounts::{
    AccountStatus, CreateGrantRequest, CreateOfflineAccountRequest, GrantLevel, LinkFlowState,
};
use fleet_api_types::agents::AgentResponse;
use fleet_api_types::auth::{LoginRequest, RegisterRequest};
use fleet_api_types::bots::{BotModeRequest, BotPatchRequest, BotResponse};
use fleet_api_types::chat::{ChatPage, ChatPageQuery, ChatSendRequest, ChatSendResponse};
use fleet_api_types::invites::CreateInviteRequest;
use fleet_api_types::mfa::CODE_MAX_CHARS;
use fleet_api_types::modes::{
    CreateModeRequest, ModeActionKind, ModeActionRequest, ModeResponse, ModeScheduleKind,
    ModeScheduleRequest, ModeStepRequest, ModeSteps, ModeVisibility, UpdateModeRequest,
};
use fleet_api_types::server_settings::{
    ChatFormatsInput, ConflictTextsInput, CreateServerSettingsRequest, ServerSettingsResponse,
};
use fleet_api_types::users::UserRole;
use fleet_api_types::{PageQuery, SecretText};
use fleet_client::{
    Client, ClientConfig, ClientError, LiveEvents, LoginStep, MemoryStore, ServerUrl, Trust,
};
use fleet_core::id::{AccountId, AgentId, BotId, ModeId};
use fleet_core::system::Clock as _;
use totp_rs::Totp;

use super::live::Live;
use super::logs::Logs;
use super::server::{OWNER, PASSWORD, Server};

/// One step that does `action`, which takes no field, every 250 ms.
fn every_quarter_second(action: ModeActionKind) -> ModeSteps {
    one_step(
        action,
        ModeScheduleRequest {
            kind: ModeScheduleKind::Every,
            interval_ms: Some(250),
            jitter_ms: Some(0),
        },
    )
}

/// One step that does `action`, which takes no field, on `schedule`.
fn one_step(action: ModeActionKind, schedule: ModeScheduleRequest) -> ModeSteps {
    ModeSteps(vec![step(action, None, schedule)])
}

/// A step that does `action` with `message`, if it sends one, on
/// `schedule`, always.
fn step(
    action: ModeActionKind,
    message: Option<&str>,
    schedule: ModeScheduleRequest,
) -> ModeStepRequest {
    ModeStepRequest {
        action: ModeActionRequest {
            kind: action,
            yaw: None,
            pitch: None,
            max_yaw: None,
            max_pitch: None,
            on: None,
            slot: None,
            message: message.map(str::to_owned),
        },
        schedule,
        probability: 100,
    }
}

/// A step that does `action`, with `message` if it sends one, on respawn
/// (ADR-0020, S20).
pub(crate) fn on_respawn(action: ModeActionKind, message: Option<&str>) -> ModeStepRequest {
    step(
        action,
        message,
        ModeScheduleRequest {
            kind: ModeScheduleKind::OnRespawn,
            interval_ms: None,
            jitter_ms: None,
        },
    )
}

/// Where every bot is sent: the fake connector dials nothing.
pub(crate) const MC_SERVER: &str = "mc.example.com:25565";

/// The Owner's client; see the [module docs](self).
#[derive(Debug)]
pub(crate) struct Owner {
    client: Client<MemoryStore>,
    /// Their authenticator, for a later login ([`Owner::log_in_again`]).
    app: Totp,
}

impl Owner {
    /// The Owner's first login on `server`: the setup step, the
    /// authenticator, and the login with the next step's code.
    pub(crate) async fn log_in(server: &Server) -> Self {
        let (client, app) = logged_in(server, OWNER).await;
        Self { client, app }
    }

    /// A new login with the same password and authenticator, on a client
    /// that holds nothing: the server opens the Owner's sealed TOTP secret
    /// for it, so the vault still opens what it stored (P13.5). The clock
    /// moves one TOTP step first, since the server takes only a later step
    /// than the last one used.
    pub(crate) async fn log_in_again(&mut self, server: &Server) {
        server.clock().advance(Duration::from_secs(30));
        let client = client(server);
        let step = client.login(&login(OWNER)).await.unwrap();
        assert!(matches!(step, LoginStep::MfaRequired { .. }), "{step:?}");
        client.login_mfa(code(server, &self.app)).await.unwrap();
        self.client = client;
    }

    /// Gives `username` `level` on `bot`'s account, as its owner shares it.
    pub(crate) async fn grant(&self, bot: BotId, username: &str, level: GrantLevel) {
        let account = self.shown(bot).await.account.id;
        self.client
            .create_grant(
                account,
                &CreateGrantRequest {
                    username: SecretText::new(username.to_owned()),
                    level,
                },
            )
            .await
            .unwrap();
    }

    /// A new offline account `name`, and its bot pointed at [`MC_SERVER`],
    /// stopped.
    pub(crate) async fn bot(&self, name: &str) -> BotId {
        let account = self
            .client
            .create_offline_account(&CreateOfflineAccountRequest {
                name: SecretText::new(name.to_owned()),
            })
            .await
            .unwrap();
        let bot = self.bot_of(account.id).await;
        self.client
            .patch_bot(
                bot,
                &BotPatchRequest {
                    server_address: MC_SERVER.to_owned(),
                },
            )
            .await
            .unwrap();
        bot
    }

    /// Links a Microsoft account, which the case's fake Microsoft approves
    /// at once (P13.5), and points its bot at [`MC_SERVER`], stopped: the
    /// bot. The link's poller logs its steps, which wake `logs`' wait.
    pub(crate) async fn link(&self, logs: &Logs) -> BotId {
        let started = self.client.start_link().await.unwrap();
        let account = asked(logs, "the account is linked", async || {
            let status = self.client.link_status(started.flow_id).await.unwrap();
            match status.status.known() {
                Some(LinkFlowState::Linked) => {
                    Some(status.account_id.expect("a linked flow names its account"))
                }
                Some(LinkFlowState::Waiting | LinkFlowState::Finishing) | None => None,
                Some(other) => panic!("the link ended {other:?}"),
            }
        })
        .await;
        let bot = self.bot_of(account).await;
        self.point(bot, MC_SERVER).await;
        bot
    }

    /// The status of the account `bot` plays as, as the Owner's list shows
    /// it.
    pub(crate) async fn account_status(&self, bot: BotId) -> Option<AccountStatus> {
        let account = self.shown(bot).await.account.id;
        let page = self
            .client
            .accounts(&PageQuery {
                before: None,
                limit: None,
            })
            .await
            .unwrap();
        page.items
            .into_iter()
            .find(|listed| listed.id == account)
            .and_then(|listed| listed.status.known())
    }

    /// The bot of `account`, from the Owner's list.
    async fn bot_of(&self, account: AccountId) -> BotId {
        let mut before = None;
        loop {
            let page = self
                .client
                .bots(&PageQuery {
                    before,
                    limit: None,
                })
                .await
                .unwrap();
            if let Some(bot) = page.items.iter().find(|bot| bot.account.id == account) {
                return bot.id;
            }
            before = Some(page.next.expect("the account's bot is listed"));
        }
    }

    /// A new private mode `name` of the Owner's, whose one step does
    /// `action` every 250 ms.
    pub(crate) async fn mode(&self, name: &str, action: ModeActionKind) -> ModeResponse {
        self.client
            .create_mode(&CreateModeRequest {
                name: name.to_owned(),
                visibility: ModeVisibility::Private,
                steps: every_quarter_second(action),
            })
            .await
            .unwrap()
    }

    /// A new private mode `name` of the Owner's, whose one step does
    /// `action` on respawn (ADR-0020, S20).
    pub(crate) async fn mode_on_respawn(&self, name: &str, action: ModeActionKind) -> ModeResponse {
        self.mode_of(name, ModeSteps(vec![on_respawn(action, None)]))
            .await
    }

    /// A new private mode `name` of the Owner's with `steps`.
    pub(crate) async fn mode_of(&self, name: &str, steps: ModeSteps) -> ModeResponse {
        self.client
            .create_mode(&CreateModeRequest {
                name: name.to_owned(),
                visibility: ModeVisibility::Private,
                steps,
            })
            .await
            .unwrap()
    }

    /// Points `bot` at the Minecraft server `address`.
    pub(crate) async fn point(&self, bot: BotId, address: &str) {
        self.client
            .patch_bot(
                bot,
                &BotPatchRequest {
                    server_address: address.to_owned(),
                },
            )
            .await
            .unwrap();
    }

    /// `mode` edited so that its one step does `action` every 250 ms
    /// instead, at the version it was answered with.
    pub(crate) async fn edit_mode(
        &self,
        mode: &ModeResponse,
        action: ModeActionKind,
    ) -> ModeResponse {
        self.client
            .update_mode(
                mode.id,
                &UpdateModeRequest {
                    version: mode.version,
                    name: mode.name.as_str().to_owned(),
                    visibility: ModeVisibility::Private,
                    steps: every_quarter_second(action),
                },
            )
            .await
            .unwrap()
    }

    /// Gives the Minecraft server `server` the conflict texts `texts`, as an
    /// Admin or the Owner does (P11.9; ADR-0020, S77).
    pub(crate) async fn server_settings(
        &self,
        server: &str,
        texts: &[&str],
    ) -> ServerSettingsResponse {
        self.client
            .create_server_settings(&CreateServerSettingsRequest {
                server: server.to_owned(),
                conflict_texts: ConflictTextsInput(
                    texts.iter().map(|text| (*text).to_owned()).collect(),
                ),
                chat_formats: ChatFormatsInput::default(),
            })
            .await
            .unwrap()
    }

    /// Switches `bot` to `mode`.
    pub(crate) async fn set_mode(&self, bot: BotId, mode: ModeId) {
        self.client
            .set_bot_mode(bot, &BotModeRequest { mode_id: mode })
            .await
            .unwrap();
    }

    /// Starts `bot`.
    pub(crate) async fn start(&self, bot: BotId) {
        self.client.start_bot(bot).await.unwrap();
    }

    /// Stops `bot`.
    pub(crate) async fn stop(&self, bot: BotId) {
        self.client.stop_bot(bot).await.unwrap();
    }

    /// Disables `agent` for good, as the agents' screen does.
    pub(crate) async fn disable_agent(&self, agent: AgentId) -> AgentResponse {
        self.client.disable_agent(agent).await.unwrap()
    }

    /// Releases disabled `agent`'s bots, as the agents' screen does.
    pub(crate) async fn release_agent(&self, agent: AgentId) -> AgentResponse {
        self.client.release_agent(agent).await.unwrap()
    }

    /// Has `bot` send `text` through `POST /bots/{id}/chat`.
    pub(crate) async fn chat(
        &self,
        bot: BotId,
        text: &str,
    ) -> Result<ChatSendResponse, ClientError> {
        self.client
            .send_chat(
                bot,
                &ChatSendRequest {
                    message: text.to_owned(),
                },
            )
            .await
    }

    /// `bot` as `GET /bots/{id}` shows it to the Owner now.
    pub(crate) async fn shown(&self, bot: BotId) -> BotResponse {
        self.client.bot(bot).await.unwrap()
    }

    /// The Owner's live events, read by a task of their own (S58).
    pub(crate) fn live(&self) -> Live {
        Live::open(self.client.live_events())
    }

    /// The newest page of `bot`'s chat, as this user may see it.
    pub(crate) async fn history(&self, bot: BotId) -> ChatPage {
        self.client
            .chat(
                bot,
                &ChatPageQuery {
                    before: None,
                    limit: None,
                },
            )
            .await
            .unwrap()
    }
}

/// A Member a case invited (P13.4): registered with an invite of the
/// Owner's, and logged in with 2FA as the Owner is.
#[derive(Debug)]
pub(crate) struct Member {
    /// Their username.
    pub(crate) name: String,
    client: Client<MemoryStore>,
}

impl Member {
    /// A new Member `name`, registered with a fresh invite of `owner`'s.
    pub(crate) async fn join(server: &Server, owner: &Owner, name: &str) -> Self {
        let invite = owner
            .client
            .create_invite(&CreateInviteRequest {
                role: UserRole::Member,
            })
            .await
            .unwrap();
        client(server)
            .register(&RegisterRequest {
                invite: invite.code,
                username: SecretText::new(name.to_owned()),
                password: SecretText::new(PASSWORD.to_owned()),
            })
            .await
            .unwrap();
        Self {
            name: name.to_owned(),
            client: logged_in(server, name).await.0,
        }
    }

    /// Their live events, for the case to read as it likes.
    pub(crate) fn live_events(&self) -> LiveEvents<MemoryStore> {
        self.client.live_events()
    }
}

/// A client of `server` with no session.
fn client(server: &Server) -> Client<MemoryStore> {
    Client::new(
        ClientConfig {
            server: ServerUrl::parse(&server.url()).unwrap(),
            trust: Trust::System,
        },
        MemoryStore::new(),
    )
    .unwrap()
}

/// How long [`asked`] leaves between two questions: three a second, under
/// the ten a second the API allows one user.
const ASK_EVERY: Duration = Duration::from_millis(300);

/// Asks the API `ask` until it answers, at `logs`' wakes but no more often
/// than every [`ASK_EVERY`]: a wait that reads through the API would
/// otherwise spend the user's rate limit on a busy log. Bounded as every
/// wait of `logs` is.
pub(crate) async fn asked<T>(
    logs: &Logs,
    what: &str,
    mut ask: impl AsyncFnMut() -> Option<T>,
) -> T {
    let mut last = None::<std::time::Instant>;
    let mut answer = None;
    logs.until(what, async || {
        if last.is_some_and(|at| at.elapsed() < ASK_EVERY) {
            return false;
        }
        last = Some(std::time::Instant::now());
        answer = ask().await;
        answer.is_some()
    })
    .await;
    answer.expect("the wait ended with an answer")
}

/// `username`'s first login on `server`: the setup step, the authenticator,
/// and the login with the next step's code.
async fn logged_in(server: &Server, username: &str) -> (Client<MemoryStore>, Totp) {
    let client = client(server);
    let step = client.login(&login(username)).await.unwrap();
    assert!(matches!(step, LoginStep::SetupRequired { .. }), "{step:?}");
    let enrolled = client.mfa_enroll().await.unwrap();
    let app = Totp::from_url(enrolled.otpauth_uri.expose()).unwrap();
    client.mfa_confirm(code(server, &app)).await.unwrap();
    // The next TOTP step, so a new code logs in.
    server.clock().advance(Duration::from_secs(30));
    let step = client.login(&login(username)).await.unwrap();
    assert!(matches!(step, LoginStep::MfaRequired { .. }), "{step:?}");
    client.login_mfa(code(server, &app)).await.unwrap();
    (client, app)
}

fn login(username: &str) -> LoginRequest {
    LoginRequest {
        username: SecretText::new(username.to_owned()),
        password: SecretText::new(PASSWORD.to_owned()),
    }
}

/// The authenticator's code at the server's time.
fn code(server: &Server, app: &Totp) -> SecretText<CODE_MAX_CHARS> {
    let now = u64::try_from(server.clock().now().timestamp()).unwrap();
    SecretText::new(app.generate(now).to_string())
}
