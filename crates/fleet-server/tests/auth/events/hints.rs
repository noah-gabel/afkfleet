//! The server's own hints to the live events sockets (ADR-0020, S42, R36):
//! every write that changes what a bot reads as tells the sockets which
//! bots, once its commit returned; a write that changes nothing tells
//! nothing. The bots' writes, a user's disable and enable, and an agent's
//! disable here; the accounts', grants', links' and session tokens' in the
//! accounts tests; the agents' streams and the reconciler in their own.

use std::collections::BTreeSet;

use fleet_core::audit::AuditAction;
use fleet_core::authz::Role;
use fleet_core::id::{AgentId, BotId};
use fleet_core::mode::ModePreset;
use fleet_server::ports::store::Store as _;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::super::bots::failed_at;
use super::super::support::{Hint, Setup, USERS, setup};

const BOTS: &str = "/api/v1/bots";

/// Hints, each with the commit count at its call.
type Heard = Vec<(u64, Hint)>;

/// The commit count now.
fn count(setup: &Setup) -> u64 {
    *setup.db.commits().borrow()
}

/// The hints since the last call, each with its commit count: one for
/// `bots` at the count now, after the call's commit, or none.
fn heard_after(setup: &Setup, bots: &[BotId]) -> (Heard, Heard) {
    let expected = if bots.is_empty() {
        Vec::new()
    } else {
        vec![(count(setup), Hint::Bots(bots.to_vec()))]
    };
    (setup.heard.take(), expected)
}

/// `bot` when `hinted`, else none.
fn only(hinted: bool, bot: &BotId) -> &[BotId] {
    if hinted {
        core::slice::from_ref(bot)
    } else {
        &[]
    }
}

#[sqlx::test(migrations = false)]
async fn each_bot_write_hints_its_bot_after_its_commit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let (_, bot) = setup.bot_of(member.id, 10).await;
    let action = |name: &str| format!("{BOTS}/{bot}/{name}");
    setup.heard.take();

    let mut steps = Vec::new();
    for (name, path, hinted) in [
        ("start", action("start"), true),
        ("start again", action("start"), false),
        ("stop", action("stop"), true),
    ] {
        let seen = setup.post(&path, Some(&member.token), None).await;
        assert_eq!(seen.status, 200, "{name}: {}", seen.text);
        steps.push((name, heard_after(&setup, only(hinted, &bot))));
    }
    let farm = ModePreset::Farm.id().to_string();
    for (name, hinted) in [("a mode", true), ("the same mode", false)] {
        let seen = setup
            .request(
                axum::http::Method::PUT,
                &action("mode"),
                Some(&member.token),
                Some(json!({ "mode_id": farm })),
            )
            .await;
        assert_eq!(seen.status, 200, "{name}: {}", seen.text);
        steps.push((name, heard_after(&setup, only(hinted, &bot))));
    }
    setup.stamp(member.session).await;
    setup.heard.take();
    for (name, hinted) in [("a server", true), ("the same server", false)] {
        let seen = setup
            .patch(
                &format!("{BOTS}/{bot}"),
                Some(&member.token),
                json!({ "server_address": "mc.example.com" }),
            )
            .await;
        assert_eq!(seen.status, 200, "{name}: {}", seen.text);
        steps.push((name, heard_after(&setup, only(hinted, &bot))));
    }

    for (name, (heard, expected)) in steps {
        assert_eq!(heard, expected, "{name}");
    }
}

#[sqlx::test(migrations = false)]
async fn clearing_a_resting_bots_sticky_state_hints_it_after_its_commit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let (_, bot) = setup.bot_of(member.id, 10).await;
    setup.rested(bot, 3, failed_at(1000)).await;
    setup.heard.take();

    let reset = setup
        .post(&format!("{BOTS}/{bot}/reset"), Some(&member.token), None)
        .await;

    assert_eq!(reset.status, 200, "{}", reset.text);
    let (heard, expected) = heard_after(&setup, &[bot]);
    assert_eq!(heard, expected);
}

#[sqlx::test(migrations = false)]
async fn disabling_or_enabling_a_user_hints_their_bots_after_its_commit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let the_owner = setup.person(6, "theowner", Role::Owner).await;
    let member = setup.person(2, "member", Role::Member).await;
    let (_, first) = setup.bot_of(member.id, 10).await;
    let (_, second) = setup.bot_of(member.id, 11).await;
    let bots: Vec<BotId> = BTreeSet::from([first, second]).into_iter().collect();
    setup.stamp(the_owner.session).await;
    setup.heard.take();
    let path = format!("{USERS}/{}", member.id);

    let mut steps = Vec::new();
    for (name, disabled, hinted) in [
        ("disabled", true, true),
        ("disabled again", true, false),
        ("enabled", false, true),
    ] {
        let seen = setup
            .patch(
                &path,
                Some(&the_owner.token),
                json!({ "disabled": disabled }),
            )
            .await;
        assert_eq!(seen.status, 200, "{name}: {}", seen.text);
        steps.push((name, heard_after(&setup, if hinted { &bots } else { &[] })));
    }

    for (name, (heard, expected)) in steps {
        assert_eq!(heard, expected, "{name}");
    }
}

#[sqlx::test(migrations = false)]
async fn disabling_an_agent_hints_its_bots_after_its_commit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(7, "admin", Role::Admin).await;
    let member = setup.person(2, "member", Role::Member).await;
    let (_, bot) = setup.bot_of(member.id, 10).await;
    let agent: AgentId = setup.assigned(bot, 3, None).await;
    setup.stamp(admin.session).await;
    setup.heard.take();
    let disable = || async {
        setup
            .post(
                &format!("/api/v1/agents/{agent}/disable"),
                Some(&admin.token),
                None,
            )
            .await
    };

    let first = disable().await;
    let after_first = heard_after(&setup, &[bot]);
    let again = disable().await;
    let after_again = heard_after(&setup, &[]);

    assert_eq!((first.status, again.status), (200, 200));
    assert_eq!(after_first.0, after_first.1);
    assert_eq!(after_again.0, after_again.1, "a disabled agent's disable");
}

/// What each audited write tells the live events sockets, so no new action
/// goes unconsidered: `bots` when what a bot reads as may change, `mode`
/// for a mode's writes, `role` for a role change, `chat` for a user's line
/// (published by its send), or nothing, with why.
#[test]
fn every_audit_action_is_labelled_with_what_it_tells_the_sockets() {
    let labels: &[(&str, &str)] = &[
        ("user.create", "nothing: a new user has no bots"),
        ("security.refresh_token_reuse", "nothing: a session"),
        ("security.lockout", "nothing: a login's counter"),
        ("mfa.enroll", "nothing: a user's second factor"),
        ("mfa.reset", "nothing: a user's second factor"),
        ("auth.login", "nothing: a session"),
        ("auth.logout", "nothing: a session"),
        ("auth.step_up", "nothing: a session"),
        ("user.set_role", "role"),
        ("user.set_disabled", "bots"),
        ("invite.create", "nothing: an invite"),
        ("invite.revoke", "nothing: an invite"),
        ("invite.use", "nothing: an invite"),
        ("user.change_password", "nothing: a password"),
        ("session.revoke", "nothing: a session"),
        ("vault.rotate_key", "nothing: sealed envelopes, never shown"),
        ("account.refresh", "nothing: a token, never shown"),
        ("account.set_status", "bots"),
        ("account.rename", "bots"),
        ("security.account_profile_mismatch", "bots"),
        ("account.start_link", "nothing: a flow has no bot yet"),
        ("account.link", "bots"),
        ("account.approve", "bots"),
        ("account.refuse", "bots"),
        ("account.delete", "bots"),
        ("account.relink", "bots"),
        ("grant.create", "bots"),
        ("grant.set_level", "bots"),
        ("grant.revoke", "bots"),
        ("enrollment_token.create", "nothing: a token"),
        ("enrollment_token.use", "nothing: a new agent has no bots"),
        ("agent.enroll", "nothing: a new agent has no bots"),
        ("agent.connect", "bots"),
        ("agent.disconnect", "bots"),
        (
            "security.agent_identity_conflict",
            "nothing: a refused stream",
        ),
        ("agent.disable", "bots"),
        ("bot.assign", "bots"),
        ("bot.unassign", "bots"),
        ("bot.detach", "bots"),
        ("bot.stray", "nothing: a fence no read shows"),
        ("agent.release", "bots"),
        ("bot.set_server", "bots"),
        ("bot.set_mode", "bots"),
        ("bot.start", "bots"),
        ("bot.stop", "bots"),
        (
            "bot.restart",
            "nothing: sent to the stream, whose report follows",
        ),
        (
            "bot.reset",
            "bots: a sticky state cleared; a sent one's report follows",
        ),
        (
            "bot.resume",
            "bots: a sticky state cleared; a sent one's report follows",
        ),
        ("bot.send_chat", "chat"),
        ("bot.send_command", "chat"),
        ("mode.create", "mode"),
        ("mode.update", "mode"),
        ("mode.delete", "mode"),
    ];

    let labelled: Vec<&str> = labels.iter().map(|(name, _)| *name).collect();
    let actions: Vec<&str> = AuditAction::ALL.iter().map(AuditAction::as_str).collect();

    assert_eq!(
        labelled, actions,
        "label every action, in AuditAction::ALL's order"
    );
    assert!(
        labels
            .iter()
            .all(|(_, told)| ["bots", "mode", "role", "chat", "nothing"]
                .iter()
                .any(|kind| told.starts_with(kind))),
        "each says what it tells"
    );
}
