//! What a bot sees, on demand (Plan.md P16.20; ADR-0036, M14):
//! `GET /api/v1/bots/{id}/screen` for anyone who may see the bot, 404
//! below; its own per-user limit, spent after authorization; never a hang:
//! a bot that isn't online and an agent of the last release are answered at
//! once without asking, and an agent at level 2 is asked on the stream that
//! holds the bot, which alone answers. Time isn't paused (sqlx).

use core::num::NonZeroU32;
use core::time::Duration;

use fleet_core::authz::{GrantLevel, Role};
use fleet_core::bot::BotState;
use fleet_core::id::BotId;
use fleet_core::mc::MenuView;
use fleet_core::script::Identifier;
use fleet_core::script::menu::{BotInventory, Item, ItemData, Screen, ScreenKind};
use fleet_proto::control::{AgentMessage, ScreenAnswer, ScreenOutcome, Seen, ServerMessage};
use fleet_server::config::{Rate, RateLimitConfig};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::super::support::{Options, Person, Seen as Answered, Setup, T0, at, setup, setup_with};
use super::{BOTS, Stream};

fn screen_path(bot: BotId) -> String {
    format!("{BOTS}/{bot}/screen")
}

impl Setup {
    async fn look(&self, bot: BotId, by: &Person) -> Answered {
        self.get(&screen_path(bot), Some(&by.token)).await
    }
}

/// An auction house's chest with a Turtle Master potion in slot 13 and the
/// bot's sword in its first hotbar slot, as an agent reads it.
fn auction_house() -> Seen {
    let potion = Item::new(
        Identifier::try_from("minecraft:potion").unwrap(),
        NonZeroU32::new(2).unwrap(),
        ItemData::new(Vec::new()),
    )
    .named("Potion of the Turtle Master")
    .with_lore(["Price: 1,250", "Seller: AfkBot2"])
    .with_potion(Some("minecraft:turtle_master"));
    let sword = Item::new(
        Identifier::try_from("minecraft:diamond_sword").unwrap(),
        NonZeroU32::MIN,
        ItemData::new(Vec::new()),
    )
    .with_durability(Some(1500))
    .with_enchantments([("minecraft:sharpness", 5)]);
    Seen::of(&MenuView {
        inventory: BotInventory::new(core::iter::empty()),
        screen: Some(Screen::shown(
            ScreenKind::Generic9x1,
            "§6Auction House",
            (0..45).map(|n| match n {
                4 => Some(potion.clone()),
                36 => Some(sword.clone()),
                _ => None,
            }),
            None,
        )),
        sign: None,
    })
}

/// Waits for `stream`'s screen request and answers it with `seen`, as an
/// agent at level 2 does.
async fn answer_with(setup: &Setup, stream: &mut Stream, seen: Seen) {
    let request = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match stream.queued.recv().await.map(ServerMessage::try_from) {
                Some(Ok(ServerMessage::ReadScreen(request))) => return request,
                Some(_) => {}
                None => panic!("the stream ended"),
            }
        }
    })
    .await
    .expect("the screen request comes");
    let (verdict, _) = setup.registry.admit(
        &stream.lease,
        &AgentMessage::Screen(ScreenAnswer {
            id: request.id,
            bot_id: request.bot_id,
            outcome: ScreenOutcome::Seen(Box::new(seen)),
        }),
    );
    assert_eq!(verdict, fleet_server::grpc::registry::Verdict::Pass);
}

#[sqlx::test(migrations = false)]
async fn a_look_needs_a_session(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;

    let seen = setup.get(&screen_path(bot), None).await;

    assert_eq!(seen.status, 401, "{}", seen.text);
}

/// Below View, the bot is an unknown one; a malformed ID is a path that
/// names no bot, as everywhere here: the route has no field to judge, so it
/// has no 422.
#[sqlx::test(migrations = false)]
async fn an_unknown_a_malformed_or_an_unseen_bot_is_404(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let stranger = setup.person(3, "stranger", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;
    let unknown = BotId::new_v7(at(T0), [0xAB; 10]).unwrap();

    let seen = [
        setup.look(bot, &stranger).await,
        setup.look(unknown, &owner).await,
        setup
            .get(&format!("{BOTS}/not-an-id/screen"), Some(&owner.token))
            .await,
    ];

    for answer in seen {
        assert_eq!(answer.status, 404, "{}", answer.text);
    }
}

/// Every viewer reads it, a View grant included; a bot that runs nowhere is
/// `offline`, at once, never asked.
#[sqlx::test(migrations = false)]
async fn a_viewer_of_a_bot_that_isnt_online_reads_offline(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let viewer = setup.person(3, "viewer", Role::Member).await;
    let (account, bot) = setup.bot_of(owner.id, 10).await;
    setup
        .share(account, viewer.id, GrantLevel::View, owner.id)
        .await;

    let seen = [
        setup.look(bot, &owner).await,
        setup.look(bot, &viewer).await,
    ];

    for answer in seen {
        assert_eq!(answer.status, 200, "{}", answer.text);
        assert_eq!(answer.body, json!({ "kind": "offline" }));
    }
}

/// An agent of the last release would end its stream on a message it
/// can't read: it's never sent one, and the answer says it needs the next
/// release.
#[sqlx::test(migrations = false)]
async fn a_bot_on_an_agent_of_the_last_release_is_agent_too_old_and_its_stream_sent_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;
    setup.assigned(bot, 1, None).await;
    let mut stream = setup.holding_at(1, 1, &[bot]);
    setup.report(&stream, bot, online(), None);

    let seen = setup.look(bot, &owner).await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    assert_eq!(seen.body, json!({ "kind": "agent_too_old" }));
    assert!(stream.next().is_none(), "nothing was queued");
}

fn online() -> BotState {
    BotState::Online {
        since: at(T0),
        attempt: NonZeroU32::MIN,
    }
}

/// An agent at level 2 is asked on the stream that holds the bot, and its
/// answer is what the viewer reads: each slot with its number, row and
/// column, each item with its ID, count, name, lore and components, the
/// server's text cleaned.
#[sqlx::test(migrations = false)]
async fn a_bot_on_an_agent_at_level_2_shows_what_its_agent_says_it_sees(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;
    setup.assigned(bot, 1, None).await;
    let mut stream = setup.holding_at(1, 2, &[bot]);
    setup.report(&stream, bot, online(), None);

    let (seen, ()) = tokio::join!(
        setup.look(bot, &owner),
        answer_with(&setup, &mut stream, auction_house())
    );

    assert_eq!(seen.status, 200, "{}", seen.text);
    insta::assert_json_snapshot!("bot_screen_seen", seen.body);
}

/// Each look spends the caller's own limit, once they may see the bot: a
/// stranger's look spends nothing, and another viewer's limit is theirs.
#[sqlx::test(migrations = false)]
async fn looks_past_the_callers_limit_are_429_and_a_strangers_spend_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            limits: RateLimitConfig {
                screen_reads: Rate::per_minute(nonzero(1)),
                ..RateLimitConfig::default()
            },
            ..Options::default()
        },
    )
    .await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let viewer = setup.person(3, "viewer", Role::Member).await;
    let stranger = setup.person(4, "stranger", Role::Member).await;
    let (account, bot) = setup.bot_of(owner.id, 10).await;
    setup
        .share(account, viewer.id, GrantLevel::View, owner.id)
        .await;

    let unseen = setup.look(bot, &stranger).await;
    let first = setup.look(bot, &owner).await;
    let second = setup.look(bot, &owner).await;
    let others = setup.look(bot, &viewer).await;

    assert_eq!(unseen.status, 404, "{}", unseen.text);
    assert_eq!(first.status, 200, "{}", first.text);
    assert_eq!(second.status, 429, "{}", second.text);
    assert!(second.headers.contains_key("retry-after"));
    assert_eq!(others.status, 200, "{}", others.text);
}

fn nonzero(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap()
}
