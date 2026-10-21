//! The bot routes (Plan.md P11.2, Appendix B; ADR-0019, S46–S56): what each
//! viewer sees of a bot, the server address after a step-up, the mode, and
//! the five lifecycle calls; the answers in every state S50's table names,
//! the audit entries and the reconciler's hints, the re-checks in the
//! transaction, and the limits.

use std::collections::BTreeMap;

use chrono::TimeDelta;
use fleet_core::audit::{
    AuditAction, AuditMetadata, AuditOutcome, AuditTarget, RecordedMetadata, RecordedTarget,
};
use fleet_core::authz::{CommandAllowlist, GrantLevel, Role};
use fleet_core::bot::{BotSnapshot, BotState, LastReport, LastState, Lifecycle, PauseReason};
use fleet_core::disconnect::{ConflictKind, DisconnectReason};
use fleet_core::id::{AccountId, AgentId, BotId, ModeId, UserId};
use fleet_core::mode::ModePreset;
use fleet_core::pki::CertFingerprint;
use fleet_core::system::Clock as _;
use fleet_core::value::AgentName;
use fleet_proto::control::{AgentMessage, LifecycleCommand, ServerMessage};
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::grpc::registry::{Hold, HoldEntry, Lease};
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::agent_connections::StreamEnd;
use fleet_server::ports::agents::{NewAgent, NewAgentCert};
use fleet_server::ports::bots::NewBot;
use fleet_server::ports::grants::AccountGrant;
use fleet_server::ports::reconcile::Change;
use fleet_server::ports::store::Store as _;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{mpsc, oneshot};

use super::support::{
    Meanwhile, Options, Person, Seen, Setup, T0, at, redacted, setup, setup_entry, setup_with,
};

mod chat;
mod history;

const BOTS: &str = "/api/v1/bots";

fn bot_path(id: BotId) -> String {
    format!("{BOTS}/{id}")
}

fn action_path(id: BotId, action: &str) -> String {
    format!("{BOTS}/{id}/{action}")
}

/// The stored report of a duplicate login's pause, at `millis` past `T0`.
fn paused_at(millis: i64) -> LastReport {
    LastReport {
        state: LastState::Paused(PauseReason::Conflict {
            kind: ConflictKind::DuplicateLogin,
        }),
        at: at(T0 + millis),
    }
}

/// A Microsoft-free failure: the server rejected the session twice.
fn failed_at(millis: i64) -> LastReport {
    LastReport {
        state: LastState::Failed(fleet_core::bot::FailReason::Auth),
        at: at(T0 + millis),
    }
}

/// A live stream that holds bots: its lease, its queue and its end.
struct Stream {
    lease: Lease,
    queued: mpsc::Receiver<fleet_proto::generated::ServerMessage>,
    _ended: oneshot::Receiver<StreamEnd>,
}

impl Stream {
    /// The next message queued for the agent, read back, if any.
    fn next(&mut self) -> Option<ServerMessage> {
        let message = self.queued.try_recv().ok()?;
        Some(ServerMessage::try_from(message).unwrap())
    }
}

impl Setup {
    /// The offline account `AfkBot<n>` of `owner`, created now, with its bot
    /// (no server, `afk`, stopped).
    pub(super) async fn bot_of(&self, owner: UserId, n: u8) -> (AccountId, BotId) {
        let account = AccountId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap();
        // Minted right after its account's, as every bot is (S35).
        let bot = BotId::new_v7(at(T0 + i64::from(n)), [n.wrapping_add(100); 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.accounts()
            .insert(&McAccount {
                id: account,
                owner,
                name: format!("AfkBot{n}").parse().unwrap(),
                created_at: at(T0 + i64::from(n)),
                credential: AccountCredential::Offline,
            })
            .await
            .unwrap();
        tx.bots()
            .insert(&NewBot {
                id: bot,
                account,
                mode: ModePreset::Afk.id(),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        (account, bot)
    }

    /// A grant of `level` on `account` to `user`.
    pub(super) async fn share(
        &self,
        account: AccountId,
        user: UserId,
        level: GrantLevel,
        by: UserId,
    ) {
        let mut tx = self.db.write().await.unwrap();
        tx.grants()
            .insert(&AccountGrant {
                account,
                user,
                level,
                granted_by: by,
                granted_at: at(T0),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Gives `bot` a server and sets it running, as a user's calls would.
    async fn running(&self, bot: BotId) {
        let id = bot.to_string();
        sqlx::query!(
            "UPDATE bots SET server_address = 'mc.example.com', desired_state = 'running' WHERE id = ?",
            id
        )
        .execute(self.db.write_pool())
        .await
        .unwrap();
    }

    /// Stores `last` as `bot`'s last state, through an assignment to agent
    /// `n` (enrolled here) that's then detached: the bot runs nowhere.
    async fn rested(&self, bot: BotId, n: u8, last: LastReport) {
        let agent = self.agent(n).await;
        let mut tx = self.db.write().await.unwrap();
        assert!(tx.bots().assign(bot, agent).await.unwrap());
        assert!(tx.bots().record_state(bot, agent, &last).await.unwrap());
        assert!(tx.bots().detach(bot, agent).await.unwrap());
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Assigns `bot` to agent `n` (enrolled here), with `last` stored.
    async fn assigned(&self, bot: BotId, n: u8, last: Option<LastReport>) -> AgentId {
        let agent = self.agent(n).await;
        let mut tx = self.db.write().await.unwrap();
        assert!(tx.bots().assign(bot, agent).await.unwrap());
        if let Some(last) = last {
            assert!(tx.bots().record_state(bot, agent, &last).await.unwrap());
        }
        tx.commit(setup_entry()).await.unwrap();
        agent
    }

    /// Agent `n`, whose one certificate expires a millisecond from now, with
    /// `bot` assigned to it.
    async fn short_lived_agent(&self, n: u8, bot: BotId) -> AgentId {
        let id = AgentId::new_v7(at(T0), [n; 10]).unwrap();
        let now = self.clock.now();
        let mut tx = self.db.write().await.unwrap();
        tx.agents()
            .insert(&NewAgent {
                id,
                name: AgentName::try_from(format!("agent-{n}").as_str()).unwrap(),
                created_at: now,
            })
            .await
            .unwrap();
        tx.agents()
            .insert_cert(&NewAgentCert {
                fingerprint: CertFingerprint::from_digest([n; 32]),
                agent: id,
                not_after: now + TimeDelta::milliseconds(1),
                created_at: now,
            })
            .await
            .unwrap();
        assert!(tx.bots().assign(bot, id).await.unwrap());
        tx.commit(setup_entry()).await.unwrap();
        id
    }

    /// Agent `n`'s live stream, reconciled, holding `bots` assigned.
    fn holding(&self, n: u8, bots: &[BotId]) -> Stream {
        let (lease, queued, ended) = self.stream(n);
        self.registry
            .settle(
                lease.agent(),
                lease.generation(),
                bots.iter()
                    .map(|bot| {
                        (
                            *bot,
                            HoldEntry {
                                hold: Hold::Assigned,
                                since: self.clock.now(),
                                sent: None,
                                stopping: None,
                            },
                        )
                    })
                    .collect(),
                ServerMessage::Reconcile(Vec::new()),
            )
            .unwrap();
        let mut stream = Stream {
            lease,
            queued,
            _ended: ended,
        };
        let welcome = stream.queued.try_recv();
        assert!(welcome.is_ok(), "the welcome");
        assert!(matches!(stream.next(), Some(ServerMessage::Reconcile(_))));
        stream
    }

    /// `stream`'s report of `bot` in `state`.
    fn report(&self, stream: &Stream, bot: BotId, state: BotState, last: Option<DisconnectReason>) {
        let (verdict, _) = self.registry.admit(
            &stream.lease,
            &AgentMessage::Status(BotSnapshot {
                bot_id: bot,
                state,
                since: at(T0 + 5000),
                last_disconnect: last,
            }),
        );
        // Held: a reconciled stream's report of a bot it holds goes on to the
        // ingest (S70), which this harness has none of.
        assert!(
            matches!(
                verdict,
                fleet_server::grpc::registry::Verdict::Held { settles: false, .. }
            ),
            "{verdict:?}"
        );
    }

    async fn bot_row(&self, bot: BotId) -> fleet_server::ports::bots::Bot {
        self.db.bots().find(bot).await.unwrap().unwrap()
    }

    async fn entries(&self, action: &AuditAction) -> Vec<fleet_server::ports::audit::AuditRecord> {
        self.audit()
            .await
            .into_iter()
            .filter(|entry| entry.action == *action)
            .collect()
    }

    async fn call(&self, bot: BotId, action: &str, by: &Person) -> Seen {
        self.post(&action_path(bot, action), Some(&by.token), None)
            .await
    }

    async fn set_server(&self, bot: BotId, address: &str, by: &Person) -> Seen {
        self.patch(
            &bot_path(bot),
            Some(&by.token),
            json!({ "server_address": address }),
        )
        .await
    }

    async fn set_mode(&self, bot: BotId, mode: ModeId, by: &Person) -> Seen {
        self.request(
            axum::http::Method::PUT,
            &action_path(bot, "mode"),
            Some(&by.token),
            Some(json!({ "mode_id": mode.to_string() })),
        )
        .await
    }

    /// A mode of `owner` with `visibility`, sending `/home` at start, named
    /// `home <n>` so no two modes share a name (S13).
    async fn custom_mode(&self, n: u8, owner: UserId, visibility: &str) -> ModeId {
        let id = ModeId::new_v7(at(T0), [n; 10]).unwrap();
        let text = id.to_string();
        let owner = owner.to_string();
        let name = format!("home {n}");
        let definition = r#"{"steps":[{"action":{"type":"send_chat","message":"/home"},"schedule":{"type":"at_start"},"probability":100}]}"#;
        sqlx::query!(
            "INSERT INTO modes (id, owner_user_id, name, visibility, definition_json, version, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, 1, '2027-01-15T08:00:00.000Z', '2027-01-15T08:00:00.000Z')",
            text,
            owner,
            name,
            visibility,
            definition
        )
        .execute(self.db.write_pool())
        .await
        .unwrap();
        id
    }
}

/// `role`'s person `n`, stepped up now.
async fn stepped_up(setup: &Setup, n: u8, name: &str, role: Role) -> Person {
    let person = setup.person(n, name, role).await;
    setup.stamp(person.session).await;
    person
}

/// The IDs a bot's answer holds, as snapshots name them.
fn names(
    account: AccountId,
    bot: BotId,
    people: &[(&Person, &str)],
) -> Vec<(String, &'static str)> {
    let mut names = vec![(account.to_string(), "account"), (bot.to_string(), "bot")];
    for (person, name) in people {
        names.push((
            person.id.to_string(),
            if *name == "owner" { "owner" } else { "user" },
        ));
    }
    names
}

// --- What a viewer sees -----------------------------------------------------------------

/// The field rule (S54): the account as the accounts' routes show it, its
/// owner and since-when for Admins, the Owner and Manage holders, its server
/// for Control holders and above (PR #54's review), its agent for Admins and
/// the Owner only.
#[sqlx::test(migrations = false)]
async fn each_viewer_sees_a_bot_under_its_accounts_field_rule(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "member", Role::Member).await;
    let viewer = setup.person(3, "viewer", Role::Member).await;
    let manager = setup.person(4, "manager", Role::Member).await;
    let admin = setup.person(5, "admin", Role::Admin).await;
    let the_owner = setup.person(6, "theowner", Role::Owner).await;
    let controller = setup.person(7, "controller", Role::Member).await;
    let (account, bot) = setup.bot_of(owner.id, 10).await;
    setup
        .share(account, viewer.id, GrantLevel::View, owner.id)
        .await;
    setup
        .share(account, controller.id, GrantLevel::Control, owner.id)
        .await;
    setup
        .share(account, manager.id, GrantLevel::Manage, owner.id)
        .await;
    setup.running(bot).await;
    setup.assigned(bot, 1, None).await;

    let mut seen = BTreeMap::new();
    for (name, person) in [
        ("owner", &owner),
        ("view", &viewer),
        ("control", &controller),
        ("manage", &manager),
        ("admin", &admin),
        ("the_owner", &the_owner),
    ] {
        let answer = setup.get(&bot_path(bot), Some(&person.token)).await;
        assert_eq!(answer.status, 200, "{name}");
        seen.insert(name, answer.body);
    }

    let view = &seen["view"];
    assert_eq!(view["account"].get("owner"), None);
    assert_eq!(view["account"].get("linked_at"), None);
    assert_eq!(view.get("agent"), None);
    assert_eq!(
        view.get("server_address"),
        None,
        "View holders see no server"
    );
    for name in ["control", "manage", "owner", "admin", "the_owner"] {
        assert_eq!(seen[name]["server_address"], "mc.example.com", "{name}");
    }
    assert_eq!(
        seen["manage"]["account"]["owner"]["id"],
        owner.id.to_string()
    );
    assert_eq!(seen["owner"].get("agent"), None, "Members never see agents");
    assert_eq!(seen["admin"]["agent"]["name"], "agent-1");
    let ids = names(account, bot, &[(&owner, "owner")]);
    let mut ids = ids;
    ids.push((
        AgentId::new_v7(at(T0), [1; 10]).unwrap().to_string(),
        "agent",
    ));
    insta::assert_json_snapshot!(
        "bot_as_each_viewer_sees_it",
        redacted(&serde_json::to_value(&seen).unwrap(), &ids)
    );
}

/// Each shown state (S41, S54) as the answer's `status` carries it.
#[sqlx::test(migrations = false)]
async fn a_bots_status_reads_as_each_shown_state(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Owner).await;
    let (_, no_server) = setup.bot_of(owner.id, 10).await;
    let (_, waiting) = setup.bot_of(owner.id, 11).await;
    setup.running(waiting).await;
    setup.rested(waiting, 1, paused_at(1000)).await;
    let (_, unknown) = setup.bot_of(owner.id, 12).await;
    setup.running(unknown).await;
    setup.assigned(unknown, 2, Some(failed_at(2000))).await;
    let (_, awaiting) = setup.bot_of(owner.id, 13).await;
    let (_, reported) = setup.bot_of(owner.id, 14).await;
    setup.running(awaiting).await;
    setup.running(reported).await;
    setup.agent(3).await;
    let mut tx = setup.db.write().await.unwrap();
    let three = AgentId::new_v7(at(T0), [3; 10]).unwrap();
    assert!(tx.bots().assign(awaiting, three).await.unwrap());
    assert!(tx.bots().assign(reported, three).await.unwrap());
    tx.commit(setup_entry()).await.unwrap();
    let stream = setup.holding(3, &[awaiting, reported]);
    setup.report(
        &stream,
        reported,
        BotState::Online {
            since: at(T0 + 4000),
            attempt: core::num::NonZeroU32::new(2).unwrap(),
        },
        Some(DisconnectReason::kicked(
            Some("multiplayer.disconnect.server_shutdown"),
            "Server closed",
        )),
    );

    let mut statuses = BTreeMap::new();
    for (name, bot) in [
        ("unplaced_no_server", no_server),
        ("unplaced_awaiting_agent_paused", waiting),
        ("unknown_failed", unknown),
        ("awaiting_report", awaiting),
        ("reported_online", reported),
    ] {
        let answer = setup.get(&bot_path(bot), Some(&owner.token)).await;
        assert_eq!(answer.status, 200, "{name}");
        statuses.insert(name, answer.body["status"].clone());
    }

    insta::assert_json_snapshot!("bot_status_per_shown_state", statuses);
}

#[sqlx::test(migrations = false)]
async fn the_list_holds_exactly_the_bots_the_caller_may_see_newest_first(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let friend = setup.person(3, "friend", Role::Member).await;
    let admin = setup.person(4, "admin", Role::Admin).await;
    let the_owner = setup.person(5, "theowner", Role::Owner).await;
    let (own_account, own) = setup.bot_of(member.id, 10).await;
    let (shared_account, shared) = setup.bot_of(friend.id, 11).await;
    let (_, friends) = setup.bot_of(friend.id, 12).await;
    let (_, owners) = setup.bot_of(the_owner.id, 13).await;
    setup
        .share(shared_account, member.id, GrantLevel::Control, friend.id)
        .await;

    let ids = |answer: &Seen| -> Vec<String> {
        answer.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect()
    };
    let members = setup.get(BOTS, Some(&member.token)).await;
    let admins = setup.get(BOTS, Some(&admin.token)).await;
    let owners_list = setup.get(BOTS, Some(&the_owner.token)).await;
    let first = setup
        .get(&format!("{BOTS}?limit=1"), Some(&member.token))
        .await;
    let rest = setup
        .get(
            &format!(
                "{BOTS}?limit=1&before={}",
                first.body["next"].as_str().unwrap()
            ),
            Some(&member.token),
        )
        .await;

    assert_eq!(ids(&members), [shared.to_string(), own.to_string()]);
    assert_eq!(
        ids(&admins),
        [friends.to_string(), shared.to_string(), own.to_string()]
    );
    assert_eq!(
        ids(&owners_list),
        [
            owners.to_string(),
            friends.to_string(),
            shared.to_string(),
            own.to_string()
        ]
    );
    assert_eq!(ids(&first), [shared.to_string()]);
    assert_eq!(ids(&rest), [own.to_string()]);
    assert_eq!(rest.body.get("next"), None);
    assert_eq!(members.body["items"][0]["account"]["level"], "control");
    let page_names = vec![
        (own_account.to_string(), "own_account"),
        (own.to_string(), "own_bot"),
        (shared_account.to_string(), "shared_account"),
        (shared.to_string(), "shared_bot"),
        (member.id.to_string(), "member"),
        (friend.id.to_string(), "friend"),
    ];
    insta::assert_json_snapshot!("bot_page", redacted(&members.body, &page_names));
}

#[sqlx::test(migrations = false)]
async fn the_lists_query_is_checked(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;

    let unparsed = setup
        .get(&format!("{BOTS}?before=nope"), Some(&member.token))
        .await;
    let zero = setup
        .get(&format!("{BOTS}?limit=0"), Some(&member.token))
        .await;

    assert_eq!(unparsed.status, 400);
    assert_eq!(zero.status, 422);
}

/// Every bot route answers a bot the caller may not see like an unknown
/// one, a malformed ID too, and changes nothing.
#[sqlx::test(migrations = false)]
async fn a_bot_the_caller_may_not_see_is_404_on_every_route_like_an_unknown_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let stranger = stepped_up(&setup, 3, "stranger", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;
    let unknown = BotId::new_v7(at(T0), [99; 10]).unwrap();

    for id in [bot.to_string(), unknown.to_string(), "not-an-id".to_owned()] {
        let path = format!("{BOTS}/{id}");
        let answers = [
            setup.get(&path, Some(&stranger.token)).await,
            setup
                .patch(
                    &path,
                    Some(&stranger.token),
                    json!({ "server_address": "mc.example.com" }),
                )
                .await,
            setup
                .request(
                    axum::http::Method::PUT,
                    &format!("{path}/mode"),
                    Some(&stranger.token),
                    Some(json!({ "mode_id": ModePreset::Farm.id().to_string() })),
                )
                .await,
            setup
                .post(&format!("{path}/start"), Some(&stranger.token), None)
                .await,
            setup
                .post(&format!("{path}/stop"), Some(&stranger.token), None)
                .await,
            setup
                .post(&format!("{path}/restart"), Some(&stranger.token), None)
                .await,
            setup
                .post(&format!("{path}/reset"), Some(&stranger.token), None)
                .await,
            setup
                .post(&format!("{path}/resume"), Some(&stranger.token), None)
                .await,
        ];
        for answer in answers {
            assert_eq!(answer.status, 404, "{id}: {}", answer.text);
            assert_eq!(answer.code(), "not_found");
        }
    }
    let row = setup.bot_row(bot).await;
    assert_eq!(row.server, None);
    assert_eq!(row.desired, fleet_core::bot::DesiredRunState::Stopped);
    assert_eq!(setup.hints.take(), []);
}

#[sqlx::test(migrations = false)]
async fn a_view_holder_may_not_control_and_a_control_holder_may_not_configure(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let viewer = stepped_up(&setup, 3, "viewer", Role::Member).await;
    let controller = stepped_up(&setup, 4, "controller", Role::Member).await;
    let (account, bot) = setup.bot_of(owner.id, 10).await;
    setup
        .share(account, viewer.id, GrantLevel::View, owner.id)
        .await;
    setup
        .share(account, controller.id, GrantLevel::Control, owner.id)
        .await;

    let starts = setup.call(bot, "start", &viewer).await;
    let modes = setup.set_mode(bot, ModePreset::Farm.id(), &viewer).await;
    let configures = setup.set_server(bot, "mc.example.com", &controller).await;

    for answer in [&starts, &modes, &configures] {
        assert_eq!(answer.status, 403, "{}", answer.text);
        assert_eq!(answer.code(), "forbidden");
    }
    let row = setup.bot_row(bot).await;
    assert_eq!(row.server, None);
    assert_eq!(row.mode, ModePreset::Afk.id());
    assert_eq!(row.desired, fleet_core::bot::DesiredRunState::Stopped);
}

// --- The server address (S47, S53, S55) ---------------------------------------------------

#[sqlx::test(migrations = false)]
async fn configuring_a_bot_needs_a_fresh_step_up_and_changes_nothing_without_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;

    let without = setup.set_server(bot, "mc.example.com", &owner).await;
    setup.stamp(owner.session).await;
    setup.clock.advance(TimeDelta::minutes(6).to_std().unwrap());
    let stale = setup.set_server(bot, "mc.example.com", &owner).await;

    for answer in [&without, &stale] {
        assert_eq!(answer.status, 403);
        assert_eq!(answer.code(), "step_up_required");
    }
    assert_eq!(setup.bot_row(bot).await.server, None);
    assert_eq!(setup.entries(&AuditAction::BOT_SET_SERVER).await.len(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_manage_holder_points_a_bot_at_a_server_audited_with_the_previous_one_and_hinted(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = stepped_up(&setup, 2, "owner", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;

    let first = setup.set_server(bot, "MC.Example.com", &owner).await;
    let first_hints = setup.hints.take();
    let again = setup.set_server(bot, "mc.example.com", &owner).await;
    let again_hints = setup.hints.take();
    let moved = setup.set_server(bot, "192.0.2.10:25566", &owner).await;

    assert_eq!(first.status, 200, "{}", first.text);
    assert_eq!(first.body["server_address"], "mc.example.com");
    assert_eq!(first_hints, [Change::Store]);
    assert_eq!(again.status, 200);
    assert_eq!(again_hints, [], "the same address changes nothing");
    assert_eq!(moved.body["server_address"], "192.0.2.10:25566");
    let entries = setup.entries(&AuditAction::BOT_SET_SERVER).await;
    assert_eq!(entries.len(), 2, "one per change");
    let (latest, first) = (&entries[0], &entries[1]);
    assert_eq!(first.actor, Some(owner.id));
    assert_eq!(first.ip, Some("192.0.2.10".parse().unwrap()));
    assert_eq!(first.outcome, AuditOutcome::Success);
    assert_eq!(
        first.target,
        Some(RecordedTarget::Known(AuditTarget::Bot(bot)))
    );
    assert_eq!(
        first.metadata,
        RecordedMetadata::from(
            &AuditMetadata::new()
                .text("server_host", "mc.example.com")
                .unwrap()
        )
    );
    assert_eq!(
        latest.metadata,
        RecordedMetadata::from(
            &AuditMetadata::new()
                .text("server_host", "192.0.2.10")
                .unwrap()
                .int("server_port", 25_566)
                .unwrap()
                .text("previous_server_host", "mc.example.com")
                .unwrap()
        )
    );
}

/// A 253-character host and a port are recorded whole: the host and the
/// port go into fields of their own (S53).
#[sqlx::test(migrations = false)]
async fn the_longest_address_is_recorded_whole(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = stepped_up(&setup, 2, "owner", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;
    let label = "a".repeat(63);
    let host = format!("{label}.{label}.{label}.{}", "b".repeat(61));

    let set = setup
        .set_server(bot, &format!("{host}:25565"), &owner)
        .await;

    assert_eq!(set.status, 200, "{}", set.text);
    let entry = &setup.entries(&AuditAction::BOT_SET_SERVER).await[0];
    assert_eq!(
        entry.metadata,
        RecordedMetadata::from(
            &AuditMetadata::new()
                .text("server_host", &host)
                .unwrap()
                .int("server_port", 25_565)
                .unwrap()
        )
    );
}

#[sqlx::test(migrations = false)]
async fn an_invalid_address_is_422_on_its_field_and_never_echoed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = stepped_up(&setup, 2, "owner", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;

    let answers = [
        setup
            .set_server(bot, "secret_label.example.com", &owner)
            .await,
        setup.set_server(bot, "tcp://mc.example.com", &owner).await,
        setup
            .patch(&bot_path(bot), Some(&owner.token), json!({}))
            .await,
        setup
            .patch(
                &bot_path(bot),
                Some(&owner.token),
                json!({ "server_address": "mc.example.com", "auto_start": true }),
            )
            .await,
    ];

    assert_eq!(answers[0].status, 422);
    assert_eq!(
        answers[0].body["error"]["fields"][0]["path"],
        "server_address"
    );
    assert!(
        !answers[0].text.contains("secret_label"),
        "{}",
        answers[0].text
    );
    assert_eq!(answers[1].status, 422);
    assert_eq!(answers[2].status, 400, "no address: {}", answers[2].text);
    assert_eq!(
        answers[3].status, 400,
        "an unknown field: {}",
        answers[3].text
    );
    assert_eq!(answers[3].code(), "bad_request");
    assert_eq!(setup.bot_row(bot).await.server, None);
}

// --- The mode (S47, S55) ------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn the_lifecycle_calls_and_the_mode_switch_need_no_step_up(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;

    let mode = setup.set_mode(bot, ModePreset::Farm.id(), &owner).await;
    let start = setup.call(bot, "start", &owner).await;
    let stop = setup.call(bot, "stop", &owner).await;
    let restart = setup.call(bot, "restart", &owner).await;
    let reset = setup.call(bot, "reset", &owner).await;
    let resume = setup.call(bot, "resume", &owner).await;

    assert_eq!(mode.status, 200, "{}", mode.text);
    assert_eq!(start.status, 200);
    assert_eq!(stop.status, 200);
    for answer in [&restart, &reset, &resume] {
        assert_eq!(
            answer.status, 409,
            "never step_up_required: {}",
            answer.text
        );
    }
}

#[sqlx::test(migrations = false)]
async fn a_control_holder_switches_the_mode_audited_with_the_previous_one_and_hinted(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let controller = setup.person(3, "controller", Role::Member).await;
    let (account, bot) = setup.bot_of(owner.id, 10).await;
    setup
        .share(account, controller.id, GrantLevel::Control, owner.id)
        .await;

    let switched = setup
        .set_mode(bot, ModePreset::Farm.id(), &controller)
        .await;
    let hints = setup.hints.take();
    let again = setup
        .set_mode(bot, ModePreset::Farm.id(), &controller)
        .await;

    assert_eq!(switched.status, 200, "{}", switched.text);
    assert_eq!(switched.body["mode_id"], ModePreset::Farm.id().to_string());
    assert_eq!(hints, [Change::Store]);
    assert_eq!(again.status, 200);
    assert_eq!(setup.hints.take(), [], "the same mode changes nothing");
    let entries = setup.entries(&AuditAction::BOT_SET_MODE).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].actor, Some(controller.id));
    assert_eq!(
        entries[0].metadata,
        RecordedMetadata::from(
            &AuditMetadata::new()
                .id("mode", *ModePreset::Farm.id().as_uuid())
                .unwrap()
                .id("previous_mode", *ModePreset::Afk.id().as_uuid())
                .unwrap()
        )
    );
}

#[sqlx::test(migrations = false)]
async fn an_unknown_or_hidden_mode_is_one_422_and_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let other = setup.person(3, "other", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;
    let hidden = setup.custom_mode(1, other.id, "private").await;
    let unknown = ModeId::new_v7(at(T0), [9; 10]).unwrap();

    let hidden = setup.set_mode(bot, hidden, &owner).await;
    let unknown = setup.set_mode(bot, unknown, &owner).await;

    assert_eq!(hidden.status, 422, "{}", hidden.text);
    assert_eq!(
        hidden.body_without_request_id(),
        unknown.body_without_request_id()
    );
    assert_eq!(hidden.body["error"]["fields"][0]["path"], "mode_id");
    assert_eq!(
        hidden.body["error"]["fields"][0]["message"],
        "isn't a mode you can use"
    );
    assert_eq!(setup.bot_row(bot).await.mode, ModePreset::Afk.id());
}

/// A mode that sends a command off the allowlist needs Manage (ADR-0010).
#[sqlx::test(migrations = false)]
async fn a_mode_with_a_command_off_the_allowlist_needs_manage(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Admin).await;
    let controller = setup.person(3, "controller", Role::Member).await;
    let (account, bot) = setup.bot_of(owner.id, 10).await;
    setup
        .share(account, controller.id, GrantLevel::Control, owner.id)
        .await;
    let home = setup.custom_mode(1, owner.id, "shared").await;

    let refused = setup.set_mode(bot, home, &controller).await;
    let refused_mode = setup.bot_row(bot).await.mode;
    let managed = setup.set_mode(bot, home, &owner).await;

    assert_eq!(refused.status, 403, "{}", refused.text);
    assert_eq!(refused.code(), "forbidden");
    assert_eq!(refused_mode, ModePreset::Afk.id());
    assert_eq!(managed.status, 200, "{}", managed.text);
}

/// On the allowlist, Control is enough.
#[sqlx::test(migrations = false)]
async fn a_mode_whose_commands_are_on_the_allowlist_needs_only_control(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            allowlist: CommandAllowlist::try_new(&["/home"]).unwrap(),
            ..Options::default()
        },
    )
    .await;
    let owner = setup.person(2, "owner", Role::Admin).await;
    let controller = setup.person(3, "controller", Role::Member).await;
    let (account, bot) = setup.bot_of(owner.id, 10).await;
    setup
        .share(account, controller.id, GrantLevel::Control, owner.id)
        .await;
    let home = setup.custom_mode(1, owner.id, "shared").await;

    let switched = setup.set_mode(bot, home, &controller).await;

    assert_eq!(switched.status, 200, "{}", switched.text);
    assert_eq!(setup.bot_row(bot).await.mode, home);
}

// --- Start and stop (S48, S50, S51) -------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn start_and_stop_write_whether_the_bot_should_run_audited_once_and_hinted(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;

    let started = setup.call(bot, "start", &owner).await;
    let started_hints = setup.hints.take();
    let again = setup.call(bot, "start", &owner).await;
    let again_hints = setup.hints.take();
    let running = setup.bot_row(bot).await.desired;
    let stopped = setup.call(bot, "stop", &owner).await;

    assert_eq!(started.status, 200, "{}", started.text);
    assert_eq!(started.body["desired_state"], "running");
    assert_eq!(started_hints, [Change::Store]);
    assert_eq!(again.status, 200);
    assert_eq!(again_hints, [], "it should run already");
    assert_eq!(running, fleet_core::bot::DesiredRunState::Running);
    assert_eq!(stopped.status, 200);
    assert_eq!(stopped.body["desired_state"], "stopped");
    assert_eq!(setup.entries(&AuditAction::BOT_START).await.len(), 1);
    let stop = &setup.entries(&AuditAction::BOT_STOP).await[0];
    assert_eq!(stop.actor, Some(owner.id));
    assert_eq!(
        stop.target,
        Some(RecordedTarget::Known(AuditTarget::Bot(bot)))
    );
    assert_eq!(stop.metadata, RecordedMetadata::new(), "no state known yet");
}

/// A Start of a bot that can't run yet stores that it should, and reads
/// why it runs nowhere; its server comes later (S50, U12).
#[sqlx::test(migrations = false)]
async fn starting_a_bot_without_a_server_stores_running_and_reads_why(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;

    let started = setup.call(bot, "start", &owner).await;

    assert_eq!(started.status, 200);
    assert_eq!(started.body["desired_state"], "running");
    assert_eq!(
        started.body["status"],
        json!({ "kind": "unplaced", "state": "stopped", "why": "no_server" })
    );
}

/// Stopping a Paused bot frees its slot and keeps it Paused, so a later
/// start never kicks the human (S51).
#[sqlx::test(migrations = false)]
async fn stopping_a_paused_bot_keeps_its_pause(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;
    setup.running(bot).await;
    setup.rested(bot, 1, paused_at(1000)).await;

    let stopped = setup.call(bot, "stop", &owner).await;

    assert_eq!(stopped.status, 200);
    let row = setup.bot_row(bot).await;
    assert_eq!(row.desired, fleet_core::bot::DesiredRunState::Stopped);
    assert_eq!(row.last, Some(paused_at(1000)));
    assert_eq!(stopped.body["status"]["state"], "paused");
    assert_eq!(
        setup.entries(&AuditAction::BOT_STOP).await[0].metadata,
        RecordedMetadata::from(
            &AuditMetadata::new()
                .text("state", "paused")
                .unwrap()
                .text("state_reason", "duplicate_login")
                .unwrap()
        )
    );
}

// --- Restart, reset and resume (S48–S50) ---------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_restart_of_a_bot_that_runs_nowhere_or_should_be_stopped_is_409_not_running(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, nowhere) = setup.bot_of(owner.id, 10).await;
    setup.running(nowhere).await;
    let (_, stopped) = setup.bot_of(owner.id, 11).await;
    setup.assigned(stopped, 1, None).await;
    let _stream = setup.holding(1, &[stopped]);

    let answers = [
        setup.call(nowhere, "restart", &owner).await,
        setup.call(stopped, "restart", &owner).await,
    ];

    for answer in &answers {
        assert_eq!(answer.status, 409, "{}", answer.text);
        assert_eq!(answer.code(), "bot_not_running");
    }
    assert_eq!(
        setup.entries(&AuditAction::BOT_RESTART).await.len(),
        0,
        "refusals aren't audited"
    );
    assert_eq!(setup.hints.take(), []);
}

/// A command for a bot its agent's stream doesn't hold is refused, with
/// nothing recorded or sent: it can't fire later (S48).
#[sqlx::test(migrations = false)]
async fn a_command_for_a_bot_no_stream_holds_is_409_unreachable_and_records_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;
    setup.running(bot).await;
    setup.assigned(bot, 1, Some(paused_at(1000))).await;

    let answers = [
        setup.call(bot, "restart", &owner).await,
        setup.call(bot, "resume", &owner).await,
        setup.call(bot, "reset", &owner).await,
    ];

    for answer in &answers {
        assert_eq!(answer.status, 409, "{}", answer.text);
        assert_eq!(answer.code(), "bot_unreachable");
    }
    assert_eq!(
        setup.bot_row(bot).await.last,
        Some(paused_at(1000)),
        "the stored pause stays"
    );
    assert_eq!(
        setup
            .audit()
            .await
            .iter()
            .filter(|e| e.target == Some(RecordedTarget::Known(AuditTarget::Bot(bot))))
            .count(),
        0
    );
}

/// A command for a bot whose agent's stream still holds it but whose agent
/// isn't online any more is refused like an unknown bot's: between a
/// disable's commit and the stream's end, or while a revoked or expired
/// certificate's stream waits for the sweep (S41, S50; PR #54's review).
#[sqlx::test(migrations = false)]
async fn a_command_for_a_bot_whose_agent_isnt_online_is_409_unreachable_and_sends_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let paused = BotState::Paused {
        reason: PauseReason::Conflict {
            kind: ConflictKind::DuplicateLogin,
        },
    };
    let mut cases = Vec::new();
    for (n, how) in [(1_u8, "disabled"), (2, "revoked"), (3, "expired")] {
        let (_, bot) = setup.bot_of(owner.id, 10 + n).await;
        setup.running(bot).await;
        let agent = if how == "expired" {
            setup.short_lived_agent(n, bot).await
        } else {
            setup.assigned(bot, n, None).await
        };
        let stream = setup.holding(n, &[bot]);
        setup.report(&stream, bot, paused, None);
        cases.push((how, agent, bot, stream));
    }
    let mut tx = setup.db.write().await.unwrap();
    assert!(tx.agents().disable(cases[0].1).await.unwrap());
    assert_eq!(
        tx.agents()
            .revoke_certs(cases[1].1, setup.clock.now())
            .await
            .unwrap(),
        1
    );
    tx.commit(setup_entry()).await.unwrap();
    setup.clock.advance(core::time::Duration::from_secs(1));

    for (how, _, bot, mut stream) in cases {
        let read = setup.get(&bot_path(bot), Some(&owner.token)).await;
        let answer = setup.call(bot, "resume", &owner).await;

        assert_eq!(
            read.body["status"]["kind"], "unknown",
            "{how}: {}",
            read.text
        );
        assert_eq!(answer.status, 409, "{how}: {}", answer.text);
        assert_eq!(answer.code(), "bot_unreachable", "{how}");
        assert!(stream.next().is_none(), "{how}: nothing was sent");
    }
    assert_eq!(setup.entries(&AuditAction::BOT_RESUME).await.len(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_command_goes_to_the_stream_that_holds_the_bot_recorded_first_and_answers_202(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;
    setup.running(bot).await;
    setup.assigned(bot, 1, None).await;
    let mut stream = setup.holding(1, &[bot]);
    setup.report(
        &stream,
        bot,
        BotState::Online {
            since: at(T0),
            attempt: core::num::NonZeroU32::MIN,
        },
        None,
    );

    let restart = setup.call(bot, "restart", &owner).await;
    let sent = stream.next();
    let hints = setup.hints.take();
    let resume = setup.call(bot, "resume", &owner).await;
    let reset = setup.call(bot, "reset", &owner).await;

    assert_eq!(restart.status, 202, "{}", restart.text);
    assert_eq!(restart.body["id"], bot.to_string());
    assert!(
        matches!(
            sent,
            Some(ServerMessage::Lifecycle(LifecycleCommand {
                bot_id,
                command: Lifecycle::Restart
            })) if bot_id == bot
        ),
        "{sent:?}"
    );
    assert_eq!(hints, [Change::Store]);
    assert_eq!(resume.code(), "bot_not_paused");
    assert_eq!(reset.code(), "bot_not_failed");
    assert!(stream.next().is_none(), "nothing more was sent");
    let entry = &setup.entries(&AuditAction::BOT_RESTART).await[0];
    assert_eq!(entry.actor, Some(owner.id));
    assert_eq!(
        entry.metadata,
        RecordedMetadata::from(&AuditMetadata::new().text("state", "online").unwrap())
    );
}

/// A held bot with no report on its stream yet is judged by its stored
/// state: the restore its assignment carried (S50).
#[sqlx::test(migrations = false)]
async fn a_resume_and_a_reset_of_a_held_bot_go_to_its_stream_by_its_stored_state(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, paused) = setup.bot_of(owner.id, 10).await;
    let (_, failed) = setup.bot_of(owner.id, 11).await;
    setup.running(paused).await;
    setup.running(failed).await;
    setup.assigned(paused, 1, Some(paused_at(1000))).await;
    let one = AgentId::new_v7(at(T0), [1; 10]).unwrap();
    let mut tx = setup.db.write().await.unwrap();
    assert!(tx.bots().assign(failed, one).await.unwrap());
    assert!(
        tx.bots()
            .record_state(failed, one, &failed_at(1000))
            .await
            .unwrap()
    );
    tx.commit(setup_entry()).await.unwrap();
    let mut stream = setup.holding(1, &[paused, failed]);

    let restart = setup.call(paused, "restart", &owner).await;
    let resume = setup.call(paused, "resume", &owner).await;
    let resumed = stream.next();
    let reset = setup.call(failed, "reset", &owner).await;
    let was_reset = stream.next();

    assert_eq!(
        restart.code(),
        "bot_not_running",
        "a paused bot doesn't restart"
    );
    assert_eq!(resume.status, 202, "{}", resume.text);
    assert!(matches!(
        resumed,
        Some(ServerMessage::Lifecycle(LifecycleCommand {
            command: Lifecycle::Resume,
            ..
        }))
    ));
    assert_eq!(reset.status, 202, "{}", reset.text);
    assert!(matches!(
        was_reset,
        Some(ServerMessage::Lifecycle(LifecycleCommand {
            command: Lifecycle::Reset,
            ..
        }))
    ));
    assert_eq!(
        setup.bot_row(paused).await.last,
        Some(paused_at(1000)),
        "a sent command clears nothing"
    );
    assert_eq!(
        setup.entries(&AuditAction::BOT_RESUME).await[0].metadata,
        RecordedMetadata::from(
            &AuditMetadata::new()
                .text("state", "paused")
                .unwrap()
                .text("state_reason", "duplicate_login")
                .unwrap()
                .text("effect", "sent")
                .unwrap()
        )
    );
}

/// A Resume of a Paused bot that runs nowhere, or a Reset of a Failed one,
/// clears its stored state, so its next placement starts it (S48).
#[sqlx::test(migrations = false)]
async fn a_resume_or_a_reset_of_a_bot_that_runs_nowhere_clears_its_stored_state(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, paused) = setup.bot_of(owner.id, 10).await;
    let (_, failed) = setup.bot_of(owner.id, 11).await;
    setup.rested(paused, 1, paused_at(1000)).await;
    setup.rested(failed, 2, failed_at(1000)).await;

    let wrong_reset = setup.call(paused, "reset", &owner).await;
    let wrong_resume = setup.call(failed, "resume", &owner).await;
    let resumed = setup.call(paused, "resume", &owner).await;
    let hints = setup.hints.take();
    let reset = setup.call(failed, "reset", &owner).await;
    let again = setup.call(paused, "resume", &owner).await;

    assert_eq!(wrong_reset.code(), "bot_not_failed");
    assert_eq!(wrong_resume.code(), "bot_not_paused");
    assert_eq!(resumed.status, 200, "{}", resumed.text);
    assert_eq!(
        resumed.body["status"]["state"], "stopped",
        "no sticky state any more"
    );
    assert_eq!(hints, [Change::Store]);
    assert_eq!(reset.status, 200);
    assert_eq!(again.code(), "bot_not_paused", "cleared already");
    assert_eq!(setup.bot_row(paused).await.last, None);
    assert_eq!(setup.bot_row(failed).await.last, None);
    assert_eq!(
        setup.entries(&AuditAction::BOT_RESET).await[0].metadata,
        RecordedMetadata::from(
            &AuditMetadata::new()
                .text("state", "failed")
                .unwrap()
                .text("state_reason", "auth")
                .unwrap()
                .text("effect", "cleared")
                .unwrap()
        )
    );
}

// --- The re-check in the transaction -------------------------------------------------------

/// A grant revoked between the handler's check and the service's
/// transaction refuses the write on what the transaction reads.
#[sqlx::test(migrations = false)]
async fn a_grant_revoked_meanwhile_refuses_the_write_and_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let controller = setup.person(3, "controller", Role::Member).await;
    let (account, bot) = setup.bot_of(owner.id, 10).await;
    setup
        .share(account, controller.id, GrantLevel::Control, owner.id)
        .await;

    setup
        .hooked
        .queue(Meanwhile::RevokeGrant(account, controller.id));
    let started = setup.call(bot, "start", &controller).await;

    assert_eq!(started.status, 404, "{}", started.text);
    assert_eq!(
        setup.bot_row(bot).await.desired,
        fleet_core::bot::DesiredRunState::Stopped
    );
    assert_eq!(setup.entries(&AuditAction::BOT_START).await.len(), 0);
    assert_eq!(setup.hints.take(), []);
}

/// A grant revoked between a write's commit and its answer: the answer is
/// 404, as for any bot the caller may no longer see, and the write stays
/// made (PR #54's review).
#[sqlx::test(migrations = false)]
async fn a_grant_revoked_right_after_the_write_is_answered_404(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let friend = setup.person(3, "friend", Role::Member).await;
    let (account, bot) = setup.bot_of(owner.id, 10).await;
    setup
        .share(account, friend.id, GrantLevel::Control, owner.id)
        .await;
    setup
        .hooked
        .queue_after_commit(Meanwhile::RevokeGrant(account, friend.id));

    let answer = setup.call(bot, "start", &friend).await;

    assert_eq!(answer.status, 404, "{}", answer.text);
    assert_eq!(answer.code(), "not_found");
    assert_eq!(
        setup.bot_row(bot).await.desired,
        fleet_core::bot::DesiredRunState::Running,
        "the start was made"
    );
}

#[sqlx::test(migrations = false)]
async fn a_session_revoked_meanwhile_is_401_and_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = stepped_up(&setup, 2, "owner", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;

    setup.hooked.queue(Meanwhile::RevokeSession(owner.session));
    let set = setup.set_server(bot, "mc.example.com", &owner).await;

    assert_eq!(set.status, 401, "{}", set.text);
    assert_eq!(setup.bot_row(bot).await.server, None);
}

// --- The limits (S52) ------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn every_bot_route_spends_the_users_request_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let one = Rate::per_second(core::num::NonZeroU32::MIN, core::num::NonZeroU32::MIN);
    let setup = setup_with(
        options,
        Options {
            limits: RateLimitConfig {
                per_user: one,
                ..RateLimitConfig::default()
            },
            ..Options::default()
        },
    )
    .await;
    let owner = stepped_up(&setup, 2, "owner", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;

    for (method, path, body) in [
        ("GET", BOTS.to_owned(), None),
        ("GET", bot_path(bot), None),
        ("GET", action_path(bot, "chat"), None),
        (
            "PATCH",
            bot_path(bot),
            Some(json!({ "server_address": "mc.example.com" })),
        ),
        (
            "PUT",
            action_path(bot, "mode"),
            Some(json!({ "mode_id": ModePreset::Farm.id().to_string() })),
        ),
        ("POST", action_path(bot, "start"), None),
        ("POST", action_path(bot, "stop"), None),
        ("POST", action_path(bot, "restart"), None),
        ("POST", action_path(bot, "reset"), None),
        ("POST", action_path(bot, "resume"), None),
        (
            "POST",
            action_path(bot, "chat"),
            Some(json!({ "message": "hello" })),
        ),
        (
            "POST",
            action_path(bot, "command"),
            Some(json!({ "message": "/spawn" })),
        ),
    ] {
        let method: axum::http::Method = method.parse().unwrap();
        let first = setup
            .request(method.clone(), &path, Some(&owner.token), body.clone())
            .await;
        let second = setup.request(method, &path, Some(&owner.token), body).await;
        assert_ne!(first.status, 429, "{path}: {}", first.text);
        assert_eq!(second.status, 429, "{path}");
        assert_eq!(second.headers["retry-after"], "1", "{path}");
        setup.clock.advance(core::time::Duration::from_secs(1));
    }
}

/// The bot's own limit: spent by every call that makes its account join a
/// server, refusals included, after the caller is authorized; never by
/// stop, the mode or the reads; one bucket per bot, shared by its users.
#[sqlx::test(migrations = false)]
async fn a_bots_joining_calls_spend_its_own_limit_and_stop_never_does(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            limits: RateLimitConfig {
                bot_actions: Rate::per_minute(core::num::NonZeroU32::new(2).unwrap()),
                ..RateLimitConfig::default()
            },
            ..Options::default()
        },
    )
    .await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let controller = setup.person(3, "controller", Role::Member).await;
    let stranger = setup.person(4, "stranger", Role::Member).await;
    let viewer = setup.person(5, "viewer", Role::Member).await;
    let (account, bot) = setup.bot_of(owner.id, 10).await;
    let (_, other) = setup.bot_of(owner.id, 11).await;
    setup
        .share(account, controller.id, GrantLevel::Control, owner.id)
        .await;
    setup
        .share(account, viewer.id, GrantLevel::View, owner.id)
        .await;

    let probes = [
        setup.call(bot, "start", &stranger).await,
        setup.call(bot, "start", &viewer).await,
    ];
    let start = setup.call(bot, "start", &owner).await;
    let restart = setup.call(bot, "restart", &controller).await;
    let refused = setup.call(bot, "resume", &owner).await;
    let stop = setup.call(bot, "stop", &owner).await;
    let mode = setup.set_mode(bot, ModePreset::Farm.id(), &owner).await;
    let read = setup.get(&bot_path(bot), Some(&owner.token)).await;
    let elsewhere = setup.call(other, "start", &owner).await;
    setup.clock.advance(core::time::Duration::from_secs(30));
    let later = setup.call(bot, "start", &owner).await;

    assert_eq!(probes[0].status, 404);
    assert_eq!(probes[1].status, 403);
    assert_eq!(start.status, 200, "{}", start.text);
    assert_eq!(
        restart.code(),
        "bot_not_running",
        "a refusal spends one too"
    );
    assert_eq!(refused.status, 429);
    assert_eq!(refused.code(), "rate_limited");
    assert_eq!(refused.headers["retry-after"], "30");
    assert_eq!(stop.status, 200);
    assert_eq!(mode.status, 200);
    assert_eq!(read.status, 200);
    assert_eq!(elsewhere.status, 200);
    assert_eq!(later.status, 200, "{}", later.text);
}
