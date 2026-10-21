//! The Owner (ADR-0019, S72): fleet-client over the server's real HTTP
//! listener, logged in with 2FA as the app does it, stepped up once, and
//! changing bots only through the API, so every change hints the
//! reconciler as a user's does. The server's clock is frozen between the
//! case's own moves, so the session and the step-up never expire.

use core::time::Duration;

use fleet_api_types::accounts::CreateOfflineAccountRequest;
use fleet_api_types::auth::{LoginRequest, StepUpRequest};
use fleet_api_types::bots::{BotModeRequest, BotPatchRequest};
use fleet_api_types::chat::{ChatPage, ChatPageQuery, ChatSendRequest, ChatSendResponse};
use fleet_api_types::mfa::CODE_MAX_CHARS;
use fleet_api_types::modes::{
    CreateModeRequest, ModeActionKind, ModeActionRequest, ModeResponse, ModeScheduleKind,
    ModeScheduleRequest, ModeStepRequest, ModeSteps, ModeVisibility, UpdateModeRequest,
};
use fleet_api_types::{PageQuery, SecretText};
use fleet_client::{Client, ClientConfig, ClientError, LoginStep, MemoryStore, ServerUrl, Trust};
use fleet_core::id::{BotId, ModeId};
use fleet_core::system::Clock as _;
use totp_rs::Totp;

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
    ModeSteps(vec![ModeStepRequest {
        action: ModeActionRequest {
            kind: action,
            yaw: None,
            pitch: None,
            max_yaw: None,
            max_pitch: None,
            on: None,
            slot: None,
            message: None,
        },
        schedule,
        probability: 100,
    }])
}

/// Where every bot is sent: the fake connector dials nothing.
pub(crate) const MC_SERVER: &str = "mc.example.com:25565";

/// The Owner's client; see the [module docs](self).
#[derive(Debug)]
pub(crate) struct Owner {
    client: Client<MemoryStore>,
}

impl Owner {
    /// The Owner's first login on `server`: the setup step, the
    /// authenticator, the login with the next step's code, and a step-up.
    pub(crate) async fn log_in(server: &Server) -> Self {
        let client = Client::new(
            ClientConfig {
                server: ServerUrl::parse(&server.url()).unwrap(),
                trust: Trust::System,
            },
            MemoryStore::new(),
        )
        .unwrap();
        let step = client.login(&login()).await.unwrap();
        assert!(matches!(step, LoginStep::SetupRequired { .. }), "{step:?}");
        let enrolled = client.mfa_enroll().await.unwrap();
        let app = Totp::from_url(enrolled.otpauth_uri.expose()).unwrap();
        client.mfa_confirm(code(server, &app)).await.unwrap();
        // The next TOTP step, so a new code logs in.
        server.clock().advance(Duration::from_secs(30));
        let step = client.login(&login()).await.unwrap();
        assert!(matches!(step, LoginStep::MfaRequired { .. }), "{step:?}");
        client.login_mfa(code(server, &app)).await.unwrap();
        client
            .step_up(&StepUpRequest {
                password: SecretText::new(PASSWORD.to_owned()),
            })
            .await
            .unwrap();
        Self { client }
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
        let mut before = None;
        let bot = loop {
            let page = self
                .client
                .bots(&PageQuery {
                    before,
                    limit: None,
                })
                .await
                .unwrap();
            if let Some(bot) = page.items.iter().find(|bot| bot.account.id == account.id) {
                break bot.id;
            }
            before = Some(page.next.expect("the account's bot is listed"));
        };
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
        self.client
            .create_mode(&CreateModeRequest {
                name: name.to_owned(),
                visibility: ModeVisibility::Private,
                steps: one_step(
                    action,
                    ModeScheduleRequest {
                        kind: ModeScheduleKind::OnRespawn,
                        interval_ms: None,
                        jitter_ms: None,
                    },
                ),
            })
            .await
            .unwrap()
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

fn login() -> LoginRequest {
    LoginRequest {
        username: SecretText::new(OWNER.to_owned()),
        password: SecretText::new(PASSWORD.to_owned()),
    }
}

/// The authenticator's code at the server's time.
fn code(server: &Server, app: &Totp) -> SecretText<CODE_MAX_CHARS> {
    let now = u64::try_from(server.clock().now().timestamp()).unwrap();
    SecretText::new(app.generate(now).to_string())
}
