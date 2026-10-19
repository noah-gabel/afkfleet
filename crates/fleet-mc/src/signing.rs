//! Whether a session's chat can go out now (Plan.md P3.7, found in group E;
//! ADR-0011).
//!
//! A server that enforces secure chat drops unsigned chat. azalea sends chat
//! unsigned until its chat-signing session is set up, which it does in the
//! background after the join: it fetches the account's certificates from
//! Mojang, then sends them to the server. A failed fetch is retried an hour
//! later, and a fetch that hangs never ends: azalea's HTTP client has no
//! timeout.
//!
//! - [`SigningPlugin`] publishes the session's [`SigningState`] every frame,
//!   from the bot's components and the server's login packet, and the
//!   commands with a message argument from the server's command tree
//!   ([`MessageCommands`]).
//! - [`kind`] tells plain chat, a command and a command with a message
//!   argument apart; [`decide`] says whether a message goes out now, waits,
//!   or can't go out. azalea signs no command, so a command with a message
//!   argument can't go out where signing is needed (ADR-0020, S2).
//! - [`signed_chat_ready`] waits on the caller's side, never on the host
//!   thread, until the message can go out or the deadline passes.

use core::time::Duration;
use std::sync::Arc;

use azalea::InGameState;
use azalea::app::{App, Plugin, Update};
use azalea::chat_signing::{ChatSigningSession, OnlyRefreshCertsAfter};
use azalea::ecs::message::MessageReader;
use azalea::ecs::query::{Has, With};
use azalea::ecs::system::{Local, Query};
use azalea::entity::LocalEntity;
use azalea::login::IsAuthenticated;
use azalea::packet::game::ReceiveGamePacketEvent;
use azalea::protocol::packets::game::ClientboundGamePacket;
use fleet_core::chat::ChatMessage;
use fleet_core::mc::SessionError;
use tokio::sync::watch;
use tokio::time::{self, Instant};

use crate::command_tree::MessageCommands;

/// Where a session's chat signing is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signing {
    /// Chat goes out unsigned, and the server takes it: an offline account, a
    /// server in offline mode, or one that doesn't enforce secure chat.
    NotNeeded,
    /// azalea is fetching the certificates, or about to.
    Pending,
    /// The chat-signing session is set up, so chat is signed.
    Ready,
    /// Fetching the certificates failed. azalea tries again an hour later.
    Failed,
}

/// What a session publishes about its chat signing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SigningState {
    pub(crate) signing: Signing,
    /// When the bot entered the game state, after which azalea starts the
    /// certificate fetch. The wait for signing is measured from here.
    pub(crate) in_game_since: Option<Instant>,
}

impl SigningState {
    /// The state before the bot is in the game: an offline account never
    /// needs signing, and an online one waits for it.
    pub(crate) const fn new(online: bool) -> Self {
        let signing = if online {
            Signing::Pending
        } else {
            Signing::NotNeeded
        };
        Self {
            signing,
            in_game_since: None,
        }
    }
}

/// Whether the server takes chat only if it's signed: the account is an
/// `online` one, the server `authenticated` the join with Mojang (azalea's
/// `IsAuthenticated`), and it `enforces` secure chat (its login packet).
#[must_use]
pub(crate) const fn needs_signing(online: bool, authenticated: bool, enforces: bool) -> bool {
    online && authenticated && enforces
}

/// How far azalea got with the chat-signing session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Certs {
    /// Fetching the certificates, or about to.
    Fetching,
    /// The certificates went to the server: azalea's `ChatSigningSession`.
    SetUp,
    /// The last fetch failed: azalea's `OnlyRefreshCertsAfter`, without a
    /// session.
    Failed,
}

impl Certs {
    /// From whether the bot has azalea's `ChatSigningSession` and
    /// `OnlyRefreshCertsAfter`. A failed refresh keeps the session it had.
    #[must_use]
    pub(crate) const fn from_components(session: bool, refresh_failed: bool) -> Self {
        if session {
            Self::SetUp
        } else if refresh_failed {
            Self::Failed
        } else {
            Self::Fetching
        }
    }
}

/// Where chat signing is: whether it's `needed`, and how far the `certs` got.
#[must_use]
pub(crate) const fn signing(needed: bool, certs: Certs) -> Signing {
    match certs {
        _ if !needed => Signing::NotNeeded,
        Certs::Fetching => Signing::Pending,
        Certs::SetUp => Signing::Ready,
        Certs::Failed => Signing::Failed,
    }
}

/// The bot's components that say where its chat signing is.
type SigningComponents = (
    Has<InGameState>,
    Has<IsAuthenticated>,
    Has<ChatSigningSession>,
    Has<OnlyRefreshCertsAfter>,
);

/// What a message is, for [`decide`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Plain chat, which azalea signs once its chat-signing session is set
    /// up.
    Chat,
    /// A command without a message argument, which needs no signature.
    Command,
    /// A command with a message argument, which azalea can't sign (ADR-0020,
    /// S2).
    CommandWithMessage,
}

/// What `message` is, given the server's `commands` with a message argument.
#[must_use]
pub(crate) fn kind(message: &ChatMessage, commands: &MessageCommands) -> Kind {
    match message.command_name() {
        None => Kind::Chat,
        Some(name) if commands.contains(name) => Kind::CommandWithMessage,
        Some(_) => Kind::Command,
    }
}

/// What `send_chat` does with a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Send it now.
    Send,
    /// Wait for the signing to be set up or to fail.
    Wait,
    /// Don't send it: the server would drop it.
    Unavailable,
}

/// What to do with a message of `kind`, given the session's `signing`: a
/// command without a message goes out as it is (azalea never signs commands,
/// ADR-0008 §7), and a pending signing counts as unavailable
/// `past_deadline`.
#[must_use]
pub(crate) const fn decide(signing: Signing, kind: Kind, past_deadline: bool) -> Decision {
    match (kind, signing) {
        (Kind::Command, _) | (_, Signing::NotNeeded) | (Kind::Chat, Signing::Ready) => {
            Decision::Send
        }
        // A command's message can't be signed even once azalea's
        // chat-signing session is set up (ADR-0020, S2).
        (Kind::CommandWithMessage, _) | (Kind::Chat, Signing::Failed) => Decision::Unavailable,
        (Kind::Chat, Signing::Pending) if past_deadline => Decision::Unavailable,
        (Kind::Chat, Signing::Pending) => Decision::Wait,
    }
}

/// When the wait for signing ends: `timeout` after the bot entered the game
/// or, if the state doesn't say when yet, after `called`. A timeout too large
/// to represent means no wait.
#[must_use]
pub(crate) fn deadline(state: SigningState, called: Instant, timeout: Duration) -> Instant {
    let base = state.in_game_since.unwrap_or(called);
    base.checked_add(timeout).unwrap_or(base)
}

/// Waits until a chat message can go out: the signing is set up or not
/// needed. Waits on `signing` without polling, and at most until the
/// [`deadline`].
///
/// # Errors
/// [`SessionError::ChatUnavailable`] if the signing failed or is still
/// pending at the deadline; [`SessionError::Closed`] if the session's App is
/// gone.
pub(crate) async fn signed_chat_ready(
    signing: &mut watch::Receiver<SigningState>,
    timeout: Duration,
) -> Result<(), SessionError> {
    let called = Instant::now();
    loop {
        let state = *signing.borrow_and_update();
        let deadline = deadline(state, called, timeout);
        match decide(state.signing, Kind::Chat, Instant::now() >= deadline) {
            Decision::Send => return Ok(()),
            Decision::Unavailable => return Err(SessionError::ChatUnavailable),
            // A new state or the deadline: decide again. `changed` is
            // cancel-safe, so the timeout loses nothing.
            Decision::Wait => match time::timeout_at(deadline, signing.changed()).await {
                Ok(Ok(())) | Err(_) => {}
                Ok(Err(_)) => return Err(SessionError::Closed),
            },
        }
    }
}

/// Publishes the session's [`SigningState`] on `state`, every frame, and the
/// commands with a message argument on `commands` whenever the server sends
/// its command tree. One App runs one session, so its system holds that
/// session's senders; they go with the App, which ends the waits of
/// [`signed_chat_ready`].
pub(crate) struct SigningPlugin {
    /// The account is an online one.
    pub(crate) online: bool,
    pub(crate) state: Arc<watch::Sender<SigningState>>,
    pub(crate) commands: Arc<watch::Sender<MessageCommands>>,
}

impl Plugin for SigningPlugin {
    fn build(&self, app: &mut App) {
        let online = self.online;
        let state = Arc::clone(&self.state);
        let commands = Arc::clone(&self.commands);
        // azalea registers the message too; registering it again is a no-op
        // (as in the liveness plugin). The login packet comes before the bot
        // spawns, and messages live for two frames, so it isn't missed.
        app.add_message::<ReceiveGamePacketEvent>().add_systems(
            Update,
            move |bots: Query<SigningComponents, With<LocalEntity>>,
                  mut packets: MessageReader<ReceiveGamePacketEvent>,
                  mut enforces: Local<Option<bool>>,
                  mut in_game_since: Local<Option<Instant>>| {
                for received in packets.read() {
                    match &*received.packet {
                        ClientboundGamePacket::Login(login) => {
                            *enforces = Some(login.enforces_secure_chat);
                        }
                        ClientboundGamePacket::Commands(tree) => {
                            commands.send_replace(MessageCommands::of(tree));
                        }
                        _ => {}
                    }
                }
                let Some((in_game, authenticated, session, failed)) = bots.iter().next() else {
                    return;
                };
                if in_game && in_game_since.is_none() {
                    *in_game_since = Some(Instant::now());
                }
                let needed = needs_signing(online, authenticated, enforces.unwrap_or(true));
                let published = SigningState {
                    signing: signing(needed, Certs::from_components(session, failed)),
                    in_game_since: *in_game_since,
                };
                state.send_if_modified(|current| {
                    let changed = *current != published;
                    *current = published;
                    changed
                });
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use azalea::InGameState;
    use azalea::chat_signing::{ChatSigningSession, OnlyRefreshCertsAfter};
    use azalea::core::entity_id::MinecraftEntityId;
    use azalea::core::game_type::{GameMode, OptionalGameType};
    use azalea::ecs::entity::Entity;
    use azalea::entity::LocalEntity;
    use azalea::login::IsAuthenticated;
    use azalea::packet::game::ReceiveGamePacketEvent;
    use azalea::protocol::packets::common::CommonPlayerSpawnInfo;
    use azalea::protocol::packets::game::c_commands::{
        BrigadierNodeStub, BrigadierParser, NodeType,
    };
    use azalea::protocol::packets::game::{
        ClientboundCommands, ClientboundGamePacket, ClientboundLogin,
    };
    use azalea::registry::DataRegistry as _;
    use azalea::registry::data::DimensionKind;
    use azalea::registry::identifier::Identifier;
    use rstest::rstest;
    use uuid::Uuid;

    const TIMEOUT: Duration = Duration::from_secs(10);

    // --- needs_signing, Certs, signing ---

    #[rstest]
    #[case::offline_account(false, true, true, false)]
    #[case::offline_mode_server(true, false, true, false)]
    #[case::server_without_secure_chat(true, true, false, false)]
    #[case::online_on_an_enforcing_server(true, true, true, true)]
    fn signing_is_needed_only_where_the_server_enforces_it(
        #[case] online: bool,
        #[case] authenticated: bool,
        #[case] enforces: bool,
        #[case] expected: bool,
    ) {
        assert_eq!(needs_signing(online, authenticated, enforces), expected);
    }

    #[rstest]
    #[case::fetching(false, false, Certs::Fetching)]
    #[case::set_up(true, false, Certs::SetUp)]
    #[case::fetch_failed(false, true, Certs::Failed)]
    #[case::refresh_failed_with_a_session(true, true, Certs::SetUp)]
    fn certs_follow_azaleas_components(
        #[case] session: bool,
        #[case] refresh_failed: bool,
        #[case] expected: Certs,
    ) {
        assert_eq!(Certs::from_components(session, refresh_failed), expected);
    }

    #[rstest]
    #[case::not_needed_while_fetching(false, Certs::Fetching, Signing::NotNeeded)]
    #[case::not_needed_after_a_failure(false, Certs::Failed, Signing::NotNeeded)]
    #[case::fetching(true, Certs::Fetching, Signing::Pending)]
    #[case::set_up(true, Certs::SetUp, Signing::Ready)]
    #[case::failed(true, Certs::Failed, Signing::Failed)]
    fn signing_follows_need_and_certs(
        #[case] needed: bool,
        #[case] certs: Certs,
        #[case] expected: Signing,
    ) {
        assert_eq!(signing(needed, certs), expected);
    }

    // --- decide ---

    #[rstest]
    #[case::not_needed(Signing::NotNeeded, false, Decision::Send)]
    #[case::ready(Signing::Ready, false, Decision::Send)]
    #[case::pending(Signing::Pending, false, Decision::Wait)]
    #[case::pending_past_the_deadline(Signing::Pending, true, Decision::Unavailable)]
    #[case::failed(Signing::Failed, false, Decision::Unavailable)]
    fn chat_waits_only_while_signing_is_pending(
        #[case] signing: Signing,
        #[case] past_deadline: bool,
        #[case] expected: Decision,
    ) {
        assert_eq!(decide(signing, Kind::Chat, past_deadline), expected);
    }

    #[rstest]
    #[case::not_needed(Signing::NotNeeded, false)]
    #[case::pending(Signing::Pending, false)]
    #[case::pending_past_the_deadline(Signing::Pending, true)]
    #[case::ready(Signing::Ready, false)]
    #[case::failed(Signing::Failed, false)]
    fn commands_without_a_message_always_go_out(
        #[case] signing: Signing,
        #[case] past_deadline: bool,
    ) {
        assert_eq!(
            decide(signing, Kind::Command, past_deadline),
            Decision::Send
        );
    }

    /// azalea signs no command, so where signing is needed, even once it's
    /// set up, the server would reject a command's message (ADR-0020, S2).
    #[rstest]
    #[case::not_needed(Signing::NotNeeded, false, Decision::Send)]
    #[case::pending(Signing::Pending, false, Decision::Unavailable)]
    #[case::pending_past_the_deadline(Signing::Pending, true, Decision::Unavailable)]
    #[case::ready(Signing::Ready, false, Decision::Unavailable)]
    #[case::failed(Signing::Failed, false, Decision::Unavailable)]
    fn a_command_with_a_message_goes_out_only_where_signing_isnt_needed(
        #[case] signing: Signing,
        #[case] past_deadline: bool,
        #[case] expected: Decision,
    ) {
        assert_eq!(
            decide(signing, Kind::CommandWithMessage, past_deadline),
            expected
        );
    }

    // --- kind ---

    #[rstest]
    #[case::chat("hello there", &["me", "msg"], Kind::Chat)]
    #[case::a_command_without_a_message("/spawn", &["me", "msg"], Kind::Command)]
    #[case::an_emote("/me waves", &["me", "msg"], Kind::CommandWithMessage)]
    #[case::a_whisper("/msg AfkBot2 hi", &["me", "msg"], Kind::CommandWithMessage)]
    #[case::a_name_the_tree_doesnt_have("/minecraft:me waves", &["me", "msg"], Kind::Command)]
    #[case::before_the_tree("/me waves", &[], Kind::Command)]
    fn a_message_is_chat_a_command_or_a_command_with_a_message(
        #[case] text: &str,
        #[case] commands: &[&str],
        #[case] expected: Kind,
    ) {
        let message: ChatMessage = text.parse().unwrap();

        assert_eq!(
            kind(&message, &MessageCommands::from_names(commands)),
            expected
        );
    }

    // --- deadline ---

    #[tokio::test(start_paused = true)]
    async fn the_deadline_runs_from_the_game_state_or_else_from_the_call() {
        let called = Instant::now();
        let in_game = called - Duration::from_secs(3);
        let entered = SigningState {
            signing: Signing::Pending,
            in_game_since: Some(in_game),
        };

        assert_eq!(deadline(entered, called, TIMEOUT), in_game + TIMEOUT);
        assert_eq!(
            deadline(SigningState::new(true), called, TIMEOUT),
            called + TIMEOUT
        );
    }

    #[test]
    fn the_state_before_the_game_depends_on_the_account() {
        assert_eq!(SigningState::new(false).signing, Signing::NotNeeded);
        assert_eq!(SigningState::new(true).signing, Signing::Pending);
        assert_eq!(SigningState::new(true).in_game_since, None);
    }

    // --- signed_chat_ready ---

    fn state(signing: Signing, in_game_since: Option<Instant>) -> SigningState {
        SigningState {
            signing,
            in_game_since,
        }
    }

    #[rstest]
    #[case::not_needed(Signing::NotNeeded, Ok(()))]
    #[case::ready(Signing::Ready, Ok(()))]
    #[case::failed(Signing::Failed, Err(SessionError::ChatUnavailable))]
    #[tokio::test(start_paused = true)]
    async fn a_settled_signing_answers_at_once(
        #[case] signing: Signing,
        #[case] expected: Result<(), SessionError>,
    ) {
        let (_tx, mut rx) = watch::channel(state(signing, Some(Instant::now())));
        let started = Instant::now();

        let ready = signed_chat_ready(&mut rx, TIMEOUT).await;

        assert_eq!(ready, expected);
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn a_pending_signing_is_waited_for_until_it_is_ready() {
        let (tx, mut rx) = watch::channel(state(Signing::Pending, Some(Instant::now())));
        let started = Instant::now();

        let (ready, ()) = tokio::join!(signed_chat_ready(&mut rx, TIMEOUT), async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            tx.send_modify(|state| state.signing = Signing::Ready);
        });

        assert_eq!(ready, Ok(()));
        assert_eq!(started.elapsed(), Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn a_pending_signing_that_fails_makes_chat_unavailable() {
        let (tx, mut rx) = watch::channel(state(Signing::Pending, Some(Instant::now())));

        let (ready, ()) = tokio::join!(signed_chat_ready(&mut rx, TIMEOUT), async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            tx.send_modify(|state| state.signing = Signing::Failed);
        });

        assert_eq!(ready, Err(SessionError::ChatUnavailable));
    }

    #[tokio::test(start_paused = true)]
    async fn a_signing_still_pending_at_the_deadline_makes_chat_unavailable() {
        let in_game = Instant::now();
        tokio::time::advance(Duration::from_secs(4)).await;
        let (_tx, mut rx) = watch::channel(state(Signing::Pending, Some(in_game)));
        let started = Instant::now();

        let ready = signed_chat_ready(&mut rx, TIMEOUT).await;

        assert_eq!(ready, Err(SessionError::ChatUnavailable));
        // The deadline runs from the game state, not from the call.
        assert_eq!(started.elapsed(), Duration::from_secs(6));
    }

    #[tokio::test(start_paused = true)]
    async fn a_signing_pending_past_the_deadline_answers_at_once() {
        let in_game = Instant::now();
        tokio::time::advance(TIMEOUT + Duration::from_secs(1)).await;
        let (_tx, mut rx) = watch::channel(state(Signing::Pending, Some(in_game)));
        let started = Instant::now();

        let ready = signed_chat_ready(&mut rx, TIMEOUT).await;

        assert_eq!(ready, Err(SessionError::ChatUnavailable));
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_game_state_time_the_wait_runs_from_the_call() {
        let (_tx, mut rx) = watch::channel(SigningState::new(true));
        let started = Instant::now();

        let ready = signed_chat_ready(&mut rx, TIMEOUT).await;

        assert_eq!(ready, Err(SessionError::ChatUnavailable));
        assert_eq!(started.elapsed(), TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn a_gone_app_ends_the_wait_as_closed() {
        let (tx, mut rx) = watch::channel(state(Signing::Pending, Some(Instant::now())));

        let (ready, ()) = tokio::join!(signed_chat_ready(&mut rx, TIMEOUT), async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            drop(tx);
        });

        assert_eq!(ready, Err(SessionError::Closed));
    }

    // --- SigningPlugin ---

    /// An App with the plugin, and the receiver of the signing state it
    /// publishes.
    fn app(online: bool) -> (App, watch::Receiver<SigningState>) {
        let (app, rx, _commands) = app_with_commands(online);
        (app, rx)
    }

    /// [`app`], and the receiver of the message commands it publishes.
    fn app_with_commands(
        online: bool,
    ) -> (
        App,
        watch::Receiver<SigningState>,
        watch::Receiver<MessageCommands>,
    ) {
        let (tx, rx) = watch::channel(SigningState::new(online));
        let (commands_tx, commands_rx) = watch::channel(MessageCommands::default());
        let mut app = App::new();
        app.add_plugins(SigningPlugin {
            online,
            state: Arc::new(tx),
            commands: Arc::new(commands_tx),
        });
        (app, rx, commands_rx)
    }

    /// A command tree with `names`, each with one message argument.
    fn commands_packet(names: &[&str]) -> ReceiveGamePacketEvent {
        let mut entries = vec![BrigadierNodeStub {
            is_executable: false,
            children: (1..=names.len())
                .map(|at| u32::try_from(2 * at - 1).unwrap())
                .collect(),
            redirect_node: None,
            node_type: NodeType::Root,
            is_restricted: false,
        }];
        for (at, name) in names.iter().enumerate() {
            entries.push(BrigadierNodeStub {
                is_executable: false,
                children: vec![u32::try_from(2 * at + 2).unwrap()],
                redirect_node: None,
                node_type: NodeType::Literal {
                    name: (*name).to_owned(),
                },
                is_restricted: false,
            });
            entries.push(BrigadierNodeStub {
                is_executable: true,
                children: Vec::new(),
                redirect_node: None,
                node_type: NodeType::Argument {
                    name: "message".to_owned(),
                    parser: BrigadierParser::Message,
                    suggestions_type: None,
                },
                is_restricted: false,
            });
        }
        ReceiveGamePacketEvent {
            entity: Entity::PLACEHOLDER,
            packet: Arc::new(ClientboundGamePacket::Commands(ClientboundCommands {
                entries,
                root_index: 0,
            })),
        }
    }

    #[test]
    fn the_servers_command_tree_publishes_its_message_commands() {
        let (mut app, _rx, commands) = app_with_commands(true);
        spawn_authenticated_bot(&mut app);

        app.world_mut()
            .write_message(commands_packet(&["me", "msg"]));
        app.update();

        assert_eq!(
            *commands.borrow(),
            MessageCommands::from_names(&["me", "msg"])
        );
    }

    #[test]
    fn a_new_command_tree_replaces_the_message_commands() {
        let (mut app, _rx, commands) = app_with_commands(true);
        spawn_authenticated_bot(&mut app);
        app.world_mut()
            .write_message(commands_packet(&["me", "msg"]));
        app.update();

        app.world_mut().write_message(commands_packet(&["say"]));
        app.update();

        assert_eq!(*commands.borrow(), MessageCommands::from_names(&["say"]));
    }

    /// A login packet that says whether the server enforces secure chat.
    fn login(enforces_secure_chat: bool) -> ReceiveGamePacketEvent {
        let packet = ClientboundGamePacket::Login(ClientboundLogin {
            player_id: MinecraftEntityId(1),
            hardcore: false,
            levels: Vec::new(),
            max_players: 20,
            chunk_radius: 8,
            simulation_distance: 8,
            reduced_debug_info: false,
            show_death_screen: true,
            do_limited_crafting: false,
            common: CommonPlayerSpawnInfo {
                dimension_type: DimensionKind::new_raw(0),
                dimension: Identifier::new("minecraft:overworld"),
                seed: 0,
                game_type: GameMode::Survival,
                previous_game_type: OptionalGameType(None),
                is_debug: false,
                is_flat: false,
                last_death_location: None,
                portal_cooldown: 0,
                sea_level: 63,
            },
            enforces_secure_chat,
        });
        ReceiveGamePacketEvent {
            entity: Entity::PLACEHOLDER,
            packet: Arc::new(packet),
        }
    }

    /// Spawns an online bot in the game, authenticated by the server.
    fn spawn_authenticated_bot(app: &mut App) -> Entity {
        app.world_mut()
            .spawn((LocalEntity, InGameState, IsAuthenticated))
            .id()
    }

    #[test]
    fn an_online_bot_in_the_game_is_pending_and_its_time_is_stamped() {
        let (mut app, rx) = app(true);
        spawn_authenticated_bot(&mut app);

        app.update();

        let published = *rx.borrow();
        assert_eq!(published.signing, Signing::Pending);
        assert!(published.in_game_since.is_some());
    }

    #[test]
    fn a_chat_signing_session_makes_it_ready() {
        let (mut app, rx) = app(true);
        let bot = spawn_authenticated_bot(&mut app);
        app.update();
        let in_game_since = rx.borrow().in_game_since;

        app.world_mut().entity_mut(bot).insert(ChatSigningSession {
            session_id: Uuid::nil(),
            messages_sent: 0,
        });
        app.update();

        assert_eq!(rx.borrow().signing, Signing::Ready);
        assert_eq!(rx.borrow().in_game_since, in_game_since);
    }

    #[test]
    fn a_failed_fetch_makes_it_failed() {
        let (mut app, rx) = app(true);
        let bot = spawn_authenticated_bot(&mut app);

        app.world_mut()
            .entity_mut(bot)
            .insert(OnlyRefreshCertsAfter {
                refresh_at: std::time::Instant::now(),
            });
        app.update();

        assert_eq!(rx.borrow().signing, Signing::Failed);
    }

    #[test]
    fn a_server_without_secure_chat_needs_no_signing() {
        let (mut app, rx) = app(true);
        spawn_authenticated_bot(&mut app);

        app.world_mut().write_message(login(false));
        app.update();

        assert_eq!(rx.borrow().signing, Signing::NotNeeded);
    }

    #[test]
    fn a_server_with_secure_chat_needs_signing() {
        let (mut app, rx) = app(true);
        spawn_authenticated_bot(&mut app);

        app.world_mut().write_message(login(true));
        app.update();

        assert_eq!(rx.borrow().signing, Signing::Pending);
    }

    #[test]
    fn an_offline_account_needs_no_signing() {
        let (mut app, rx) = app(false);
        spawn_authenticated_bot(&mut app);

        app.update();

        assert_eq!(rx.borrow().signing, Signing::NotNeeded);
    }

    #[test]
    fn before_the_game_state_no_time_is_stamped() {
        let (mut app, rx) = app(true);
        app.world_mut().spawn((LocalEntity, IsAuthenticated));

        app.update();

        assert_eq!(rx.borrow().in_game_since, None);
    }
}
