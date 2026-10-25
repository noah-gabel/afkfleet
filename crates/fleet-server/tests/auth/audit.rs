//! `GET /audit` (Plan.md P11.9, Appendix B; ADR-0020, S47, S48, R64–R66):
//! every entry, newest first, for Admins and the Owner, each as the log
//! stores it; everyone else refused; the query's rules; the limit; and a
//! read that writes nothing and tells the sockets nothing.

use core::num::NonZeroU32;

use chrono::TimeDelta;
use fleet_core::audit::{AuditAction, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::authz::Role;
use fleet_core::id::{AgentId, BotId, EnrollmentTokenId, InviteId, SessionId};
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::store::Store as _;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::support::{Options, Person, Setup, T0, at, redacted, setup, setup_with};

const AUDIT: &str = "/api/v1/audit";

impl Setup {
    /// Records `entry` as it stands.
    async fn record(&self, entry: NewAuditEntry) -> i64 {
        let tx = self.db.write().await.unwrap();
        tx.commit(entry).await.unwrap().get()
    }

    /// The log's IDs, newest first, as the store lists them.
    async fn audit_ids(&self) -> Vec<i64> {
        self.audit()
            .await
            .iter()
            .map(|entry| entry.id.get())
            .collect()
    }
}

/// The IDs a page answered, in its order.
fn ids(page: &Value) -> Vec<i64> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_i64().unwrap())
        .collect()
}

#[sqlx::test(migrations = false)]
async fn admins_and_the_owner_page_through_every_entry_newest_first(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let owner = setup.person(3, "owner", Role::Owner).await;
    let all = setup.audit_ids().await;
    let newest = all[..2].to_vec();

    let first = setup
        .get(&format!("{AUDIT}?limit=2"), Some(&admin.token))
        .await;
    let rest = setup
        .get(
            &format!("{AUDIT}?before={}&limit=100", newest[1]),
            Some(&owner.token),
        )
        .await;

    assert_eq!(first.status, 200, "{}", first.text);
    assert_eq!(ids(&first.body), newest);
    assert_eq!(first.body["next"], json!(newest[1]));
    assert_eq!(rest.status, 200, "{}", rest.text);
    assert_eq!(ids(&rest.body), all[2..]);
    assert!(rest.body.get("next").is_none(), "{}", rest.text);
}

/// A page that ends exactly at the oldest entry names no next page: the
/// one more read finds nothing older (R64).
#[sqlx::test(migrations = false)]
async fn a_full_last_page_names_no_next_page(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let all = setup.audit_ids().await;

    let whole = setup
        .get(&format!("{AUDIT}?limit={}", all.len()), Some(&admin.token))
        .await;

    assert_eq!(whole.status, 200, "{}", whole.text);
    assert_eq!(ids(&whole.body), all);
    assert!(whole.body.get("next").is_none(), "{}", whole.text);
}

#[sqlx::test(migrations = false)]
async fn a_member_may_not_read_the_audit_log(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(4, "member", Role::Member).await;

    let seen = setup.get(AUDIT, Some(&member.token)).await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "forbidden");
}

#[sqlx::test(migrations = false)]
async fn without_credentials_the_audit_log_is_401(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    let seen = setup.get(AUDIT, None).await;

    assert_eq!(seen.status, 401, "{}", seen.text);
}

#[sqlx::test(migrations = false)]
async fn a_query_that_doesnt_parse_is_400_and_a_limit_or_cursor_out_of_range_422(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;

    let mut seen = Vec::new();
    for query in [
        "before=nonsense",
        "action=auth.login",
        "limit=0",
        "limit=101",
        "before=0",
    ] {
        seen.push(
            setup
                .get(&format!("{AUDIT}?{query}"), Some(&admin.token))
                .await,
        );
    }

    let statuses: Vec<u16> = seen.iter().map(|seen| seen.status).collect();
    assert_eq!(statuses, [400, 400, 422, 422, 422]);
}

#[sqlx::test(migrations = false)]
async fn reading_the_audit_log_spends_the_users_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            limits: RateLimitConfig {
                per_user: Rate::per_second(NonZeroU32::MIN, NonZeroU32::MIN),
                ..RateLimitConfig::default()
            },
            ..Options::default()
        },
    )
    .await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    assert_eq!(setup.get(AUDIT, Some(&admin.token)).await.status, 200);

    let seen = setup.get(AUDIT, Some(&admin.token)).await;

    assert_eq!(seen.status, 429, "{}", seen.text);
}

/// Three entries, as the newest page shows them to an Admin: a user's
/// change with its IP, target and details; an agent's refused connect
/// with its IP and reason; and the retention's summary, the server's own,
/// with no actor, IP or target.
#[sqlx::test(migrations = false)]
async fn each_entry_shows_what_the_log_stores_and_the_servers_own_has_no_actor_ip_or_target(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let bot = BotId::new_v7(at(T0), [5; 10]).unwrap();
    let agent = AgentId::new_v7(at(T0), [6; 10]).unwrap();
    setup
        .record(
            NewAuditEntry::new(
                at(T0 + 1000),
                AuditAction::BOT_SET_SERVER,
                AuditOutcome::Success,
            )
            .actor(admin.id)
            .ip("192.0.2.10".parse().unwrap())
            .target(AuditTarget::Bot(bot))
            .metadata(
                AuditMetadata::new()
                    .text("address", "play.example.com")
                    .unwrap()
                    .flag("first", true)
                    .unwrap(),
            ),
        )
        .await;
    setup
        .record(
            NewAuditEntry::new(
                at(T0 + 2000),
                AuditAction::AGENT_CONNECT,
                AuditOutcome::Failure,
            )
            .ip("2001:db8::7".parse().unwrap())
            .target(AuditTarget::Agent(agent))
            .metadata(AuditMetadata::new().text("reason", "expired").unwrap()),
        )
        .await;
    setup
        .record(
            NewAuditEntry::new(
                at(T0 + 3000),
                AuditAction::RETENTION_PURGE,
                AuditOutcome::Success,
            )
            .metadata(
                AuditMetadata::new()
                    .time("since", at(T0) - TimeDelta::hours(1))
                    .unwrap()
                    .int("chat_by_age", 1200)
                    .unwrap(),
            ),
        )
        .await;

    let seen = setup
        .get(&format!("{AUDIT}?limit=3"), Some(&admin.token))
        .await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    let purge = &seen.body["items"][0];
    assert!(
        purge.get("actor").is_none() && purge.get("ip").is_none() && purge.get("target").is_none(),
        "{purge}"
    );
    let mut page = redacted(
        &seen.body,
        &[
            (admin.id.to_string(), "admin"),
            (bot.to_string(), "bot-5"),
            (agent.to_string(), "agent-6"),
        ],
    );
    // The IDs are the log's rows: in order, but not pinned.
    for item in page["items"].as_array_mut().unwrap() {
        item["id"] = json!("[row]");
    }
    page["next"] = json!("[row]");
    insta::assert_json_snapshot!("audit_page", page);
}

/// Entries naming an invite, an enrollment token and a session that no
/// longer exist (deleted, or never there): each lists as it is.
#[sqlx::test(migrations = false)]
async fn an_entry_naming_a_deleted_invite_token_or_session_still_lists(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(3, "owner", Role::Owner).await;
    let invite = InviteId::new_v7(at(T0), [7; 10]).unwrap();
    let token = EnrollmentTokenId::new_v7(at(T0), [8; 10]).unwrap();
    let session = SessionId::new_v7(at(T0), [9; 10]).unwrap();
    for (action, target) in [
        (AuditAction::INVITE_REVOKE, AuditTarget::Invite(invite)),
        (
            AuditAction::ENROLLMENT_TOKEN_CREATE,
            AuditTarget::EnrollmentToken(token),
        ),
        (AuditAction::SESSION_REVOKE, AuditTarget::Session(session)),
    ] {
        setup
            .record(
                NewAuditEntry::new(at(T0), action, AuditOutcome::Success)
                    .actor(owner.id)
                    .target(target),
            )
            .await;
    }

    let seen = setup
        .get(&format!("{AUDIT}?limit=3"), Some(&owner.token))
        .await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    let targets: Vec<&Value> = seen.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| &item["target"])
        .collect();
    assert_eq!(
        targets,
        [
            &json!({ "kind": "session", "id": session.to_string() }),
            &json!({ "kind": "enrollment_token", "id": token.to_string() }),
            &json!({ "kind": "invite", "id": invite.to_string() }),
        ]
    );
}

/// A row only a newer server would write: an action, a target kind and
/// metadata values this version doesn't know. The page still answers, the
/// names as their text and each value as text.
#[sqlx::test(migrations = false)]
async fn names_and_values_only_a_newer_server_records_list_as_text(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    sqlx::query!(
        r#"INSERT INTO audit_log (at, action, target_type, target_id, outcome, metadata_json)
           VALUES ('2027-01-15T08:00:05.000Z', 'agent.teleport', 'planet',
                   '018bcfe5-6800-7cdc-8dcd-cdcdcdcd0009', 'success',
                   '{"depth": 1.5, "far": 9007199254740993, "nested": {"a": 1}}')"#
    )
    .execute(setup.db.write_pool())
    .await
    .unwrap();

    let seen = setup
        .get(&format!("{AUDIT}?limit=1"), Some(&admin.token))
        .await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    let entry = &seen.body["items"][0];
    assert_eq!(entry["action"], json!("agent.teleport"));
    assert_eq!(entry["target"]["kind"], json!("planet"));
    assert_eq!(
        entry["metadata"],
        json!([
            { "key": "depth", "text": "1.5" },
            { "key": "far", "text": "9007199254740993" },
            { "key": "nested", "text": "{\"a\":1}" },
        ])
    );
}

/// Reading the log twice, the second within the session's touch minute:
/// the second commits nothing and puts nothing on the bus (ADR-0020, F's
/// note from D2).
#[sqlx::test(migrations = false)]
async fn reading_the_audit_log_writes_nothing_and_tells_the_sockets_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin: Person = setup.person(2, "admin", Role::Admin).await;
    assert_eq!(setup.get(AUDIT, Some(&admin.token)).await.status, 200);
    let mut bus = setup.events.subscribe();
    let commits = *setup.db.commits().borrow();

    let seen = setup.get(AUDIT, Some(&admin.token)).await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    assert_eq!(*setup.db.commits().borrow(), commits);
    assert!(bus.try_recv().is_err());
}
