//! What a bot sees, on demand (Plan.md P16.20; ADR-0036, M14): the server's
//! request and the agent's answer, what crosses of a bot's view, and the
//! rules a reader holds an agent's answer to.

use core::num::{NonZeroU32, NonZeroU64};

use fleet_core::mc::MenuView;
use fleet_core::script::Identifier;
use fleet_core::script::menu::{
    BotInventory, ClickGate, Item, ItemData, Screen, ScreenKind, StateId, TradeOffer, WindowId,
};
use fleet_proto::control::{
    AgentMessage, ScreenAnswer, ScreenOutcome, ScreenRequest, ScreenRequestId, ScreenUnseen, Seen,
    ServerMessage,
};
use fleet_proto::generated as wire;
use fleet_proto::{ConvertError, Problem};
use proptest::prelude::*;

use crate::strategies::{screen_answer, seen};
use crate::{over_the_wire, same_server_message};

const BOT: &str = "018bcfe5-6800-7bab-abab-abababababab";

fn bot() -> fleet_core::id::BotId {
    BOT.parse().unwrap()
}

fn number(n: u64) -> ScreenRequestId {
    ScreenRequestId::new(NonZeroU64::new(n).unwrap())
}

fn item(id: &str, count: u32) -> Item {
    Item::new(
        Identifier::try_from(id).unwrap(),
        NonZeroU32::new(count).unwrap(),
        ItemData::new(vec![1, 2, 3]),
    )
}

/// An answer that says what the bot sees, as the agent sends it.
fn answer(seen: Seen) -> AgentMessage {
    AgentMessage::Screen(ScreenAnswer {
        id: number(7),
        bot_id: bot(),
        outcome: ScreenOutcome::Seen(Box::new(seen)),
    })
}

/// An answer's wire form whose `Seen` is `seen`.
fn wire_answer(seen: wire::Seen) -> wire::AgentMessage {
    wire::AgentMessage {
        msg: Some(wire::agent_message::Msg::Screen(wire::ScreenAnswer {
            id: 7,
            bot_id: uuid::Uuid::parse_str(BOT).unwrap().as_bytes().to_vec(),
            answer: Some(wire::screen_answer::Answer::Seen(Box::new(seen))),
        })),
    }
}

fn read(message: wire::AgentMessage) -> Result<AgentMessage, ConvertError> {
    AgentMessage::try_from(message)
}

fn seen_of(message: Result<AgentMessage, ConvertError>) -> Seen {
    match message.unwrap() {
        AgentMessage::Screen(ScreenAnswer {
            outcome: ScreenOutcome::Seen(seen),
            ..
        }) => *seen,
        other => panic!("not what a bot sees: {other:?}"),
    }
}

fn wire_item(id: &str) -> wire::SeenItem {
    wire::SeenItem {
        id: id.to_owned(),
        count: 1,
        ..wire::SeenItem::default()
    }
}

fn wire_slot(index: u32, id: &str) -> wire::SeenSlot {
    wire::SeenSlot {
        index,
        item: Some(wire_item(id)),
    }
}

fn chest(slots: Vec<wire::SeenSlot>) -> wire::Seen {
    wire::Seen {
        screen: Some(wire::SeenScreen {
            kind: "generic_9x3".to_owned(),
            title: "Auction House".to_owned(),
            slots,
            cursor: None,
        }),
        ..wire::Seen::default()
    }
}

fn refusal(message: wire::AgentMessage) -> ConvertError {
    read(message).unwrap_err()
}

proptest! {
    #[test]
    fn a_screens_answer_crosses_as_itself(answer in screen_answer()) {
        let message = AgentMessage::Screen(answer);

        prop_assert_eq!(over_the_wire::<_, wire::AgentMessage>(message.clone()), Ok(message));
    }

    #[test]
    fn what_crosses_of_a_view_crosses_again_as_itself(seen in seen()) {
        let view = MenuView {
            inventory: seen.inventory.clone(),
            screen: seen.screen.clone(),
            sign: None,
        };

        prop_assert_eq!(Seen::of(&view).inventory, seen.inventory);
        prop_assert_eq!(Seen::of(&view).screen, seen.screen);
    }
}

#[test]
fn a_screen_request_crosses_as_itself() {
    let request = ServerMessage::ReadScreen(ScreenRequest {
        id: number(3),
        bot_id: bot(),
    });

    let back = over_the_wire::<_, wire::ServerMessage>(request.clone()).unwrap();

    assert!(same_server_message(&back, &request));
}

#[test]
fn every_reason_an_agent_cant_say_crosses_as_itself() {
    for why in ScreenUnseen::ALL {
        let message = AgentMessage::Screen(ScreenAnswer {
            id: number(1),
            bot_id: bot(),
            outcome: ScreenOutcome::Unseen(why),
        });

        assert_eq!(
            over_the_wire::<_, wire::AgentMessage>(message.clone()),
            Ok(message),
            "{why:?}"
        );
    }
}

#[test]
fn what_crosses_is_the_view_without_the_guards_window_state_data_and_trades() {
    let mut gate = ClickGate::new();
    let id = gate.opened(WindowId::new(4));
    let screen = Screen::new(
        id,
        StateId::new(9),
        ScreenKind::Merchant,
        "Trader",
        [Some(item("minecraft:emerald", 3))],
        Some(item("minecraft:bread", 1)),
    )
    .with_offers([TradeOffer::new(
        item("minecraft:emerald", 1),
        None,
        item("minecraft:bread", 6),
        false,
    )]);
    let view =
        MenuView {
            inventory: BotInventory::new((0..46).map(|index| {
                (index == 36).then(|| item("minecraft:diamond_sword", 1).named("Blade"))
            })),
            screen: Some(screen),
            sign: None,
        };

    let seen = Seen::of(&view);

    let shown = seen.screen.as_ref().unwrap();
    assert_eq!((shown.id().window().get(), shown.id().opening()), (0, 0));
    assert_eq!(shown.state(), StateId::new(0));
    assert_eq!(shown.offers(), []);
    assert_eq!(shown.title().as_str(), "Trader");
    let emerald = shown.slot(0).unwrap().item().unwrap();
    assert_eq!(emerald.id().as_str(), "minecraft:emerald");
    assert!(emerald.data().is_empty());
    assert_eq!(
        shown.cursor().map(|item| item.id().as_str()),
        Some("minecraft:bread")
    );
    let sword = seen.inventory.slot(36).unwrap().item().unwrap();
    assert_eq!(sword.name().as_str(), "Blade");
    assert!(sword.data().is_empty());
}

#[test]
fn a_slot_keeps_its_place_as_a_script_names_it() {
    let seen = seen_of(read(wire_answer(chest(vec![wire_slot(
        13,
        "minecraft:potion",
    )]))));

    let slot = seen.screen.unwrap().slot(13).unwrap().clone();
    let place = slot.place().unwrap();
    assert_eq!((place.row, place.column), (1, 4));
    assert_eq!(slot.item().unwrap().id().as_str(), "minecraft:potion");
}

#[test]
fn a_servers_text_is_cleaned_and_bounded_as_fleet_core_does_never_refused() {
    let item = wire::SeenItem {
        id: "minecraft:paper".to_owned(),
        count: 1,
        name: format!("§cPreis {}", "x".repeat(200)),
        lore: (0..40).map(|n| format!("line {n}\u{202e}")).collect(),
        ..wire::SeenItem::default()
    };
    let mut answer = chest(vec![wire::SeenSlot {
        index: 0,
        item: Some(item),
    }]);
    if let Some(screen) = answer.screen.as_mut() {
        screen.title = format!("Auction\nHouse{}", "y".repeat(500));
    }

    let seen = seen_of(read(wire_answer(answer)));

    let screen = seen.screen.unwrap();
    assert!(screen.title().as_str().starts_with("Auction"));
    assert!(screen.title().as_str().chars().count() <= Screen::MAX_TITLE);
    assert!(!screen.title().as_str().contains('\n'));
    let paper = screen.slot(0).unwrap().item().unwrap();
    assert!(paper.name().as_str().starts_with("Preis "));
    assert_eq!(paper.name().as_str().chars().count(), Item::MAX_NAME);
    assert!(paper.lore().lines().len() <= 16);
    assert!(
        paper
            .lore()
            .lines()
            .iter()
            .all(|line| !line.as_str().contains('\u{202e}'))
    );
}

#[test]
fn a_potion_and_enchantments_cross_as_ids_and_levels() {
    let mut potion = wire_item("minecraft:potion");
    potion.potion = "minecraft:turtle_master".to_owned();
    let mut sword = wire_item("minecraft:diamond_sword");
    sword.enchantments = vec![wire::SeenEnchantment {
        id: "minecraft:sharpness".to_owned(),
        level: 5,
    }];
    sword.durability = Some(1500);
    let seen = seen_of(read(wire_answer(wire::Seen {
        inventory: vec![
            wire::SeenSlot {
                index: 9,
                item: Some(potion),
            },
            wire::SeenSlot {
                index: 36,
                item: Some(sword),
            },
        ],
        ..wire::Seen::default()
    })));

    let potion = seen.inventory.slot(9).unwrap().item().unwrap();
    assert_eq!(
        potion.potion().map(Identifier::as_str),
        Some("minecraft:turtle_master")
    );
    let sword = seen.inventory.slot(36).unwrap().item().unwrap();
    assert_eq!(
        sword
            .enchantments()
            .iter()
            .map(|(id, level)| (id.as_str(), level))
            .collect::<Vec<_>>(),
        [("minecraft:sharpness", 5)]
    );
    assert_eq!(sword.durability(), Some(1500));
}

#[test]
fn a_sign_editor_crosses_with_its_place_and_side() {
    let seen = seen_of(read(wire_answer(wire::Seen {
        sign: Some(wire::SeenSign {
            x: -3,
            y: 64,
            z: 12,
            front: false,
        }),
        ..wire::Seen::default()
    })));

    let sign = seen.sign.unwrap();
    assert_eq!((sign.at.x, sign.at.y, sign.at.z), (-3, 64, 12));
    assert_eq!(sign.side, fleet_core::script::menu::SignSide::Back);
}

#[test]
fn an_answer_an_agent_never_makes_is_refused_with_its_field() {
    let cases: Vec<(wire::Seen, &str, Problem)> = vec![
        (
            chest(vec![wire_slot(63, "minecraft:stone")]),
            "SeenScreen.slots",
            Problem::OutOfRange,
        ),
        (
            chest(vec![
                wire_slot(2, "minecraft:stone"),
                wire_slot(2, "minecraft:dirt"),
            ]),
            "SeenScreen.slots",
            Problem::DuplicateSlot,
        ),
        (
            chest((0..64).map(|n| wire_slot(n, "minecraft:stone")).collect()),
            "SeenScreen.slots",
            Problem::TooMany { count: 64, max: 63 },
        ),
        (
            wire::Seen {
                inventory: vec![wire_slot(4, "minecraft:stone")],
                ..wire::Seen::default()
            },
            "Seen.inventory",
            Problem::OutOfRange,
        ),
        (
            wire::Seen {
                inventory: vec![wire_slot(46, "minecraft:stone")],
                ..wire::Seen::default()
            },
            "Seen.inventory",
            Problem::OutOfRange,
        ),
        (
            wire::Seen {
                screen: Some(wire::SeenScreen {
                    kind: "generic_10x3".to_owned(),
                    ..wire::SeenScreen::default()
                }),
                ..wire::Seen::default()
            },
            "SeenScreen.kind",
            Problem::Unknown,
        ),
        (
            chest(vec![wire_slot(1, "Not An Id")]),
            "SeenItem.id",
            Problem::NotAnIdentifier,
        ),
        (
            chest(vec![wire::SeenSlot {
                index: 1,
                item: Some(wire::SeenItem {
                    count: 0,
                    ..wire_item("minecraft:stone")
                }),
            }]),
            "SeenItem.count",
            Problem::Missing,
        ),
        (
            chest(vec![wire::SeenSlot {
                index: 1,
                item: None,
            }]),
            "SeenSlot.item",
            Problem::Missing,
        ),
    ];

    for (seen, field, problem) in cases {
        let error = refusal(wire_answer(seen));

        assert_eq!((error.field, error.problem), (field, problem));
    }
}

#[test]
fn an_answer_without_its_answer_or_its_number_is_refused() {
    let id = uuid::Uuid::parse_str(BOT).unwrap().as_bytes().to_vec();
    let message = |answer: wire::ScreenAnswer| wire::AgentMessage {
        msg: Some(wire::agent_message::Msg::Screen(answer)),
    };

    let missing = refusal(message(wire::ScreenAnswer {
        id: 1,
        bot_id: id.clone(),
        answer: None,
    }));
    let unspecified = refusal(message(wire::ScreenAnswer {
        id: 1,
        bot_id: id.clone(),
        answer: Some(wire::screen_answer::Answer::Unseen(
            wire::ScreenUnseen::Unspecified.into(),
        )),
    }));
    let unnumbered = refusal(message(wire::ScreenAnswer {
        id: 0,
        bot_id: id,
        answer: Some(wire::screen_answer::Answer::Unseen(
            wire::ScreenUnseen::Busy.into(),
        )),
    }));

    assert_eq!(
        (missing.field, missing.problem),
        ("ScreenAnswer.answer", Problem::Missing)
    );
    assert_eq!(
        (unspecified.field, unspecified.problem),
        ("ScreenAnswer.unseen", Problem::Missing)
    );
    assert_eq!(
        (unnumbered.field, unnumbered.problem),
        ("ScreenAnswer.id", Problem::Missing)
    );
}

#[test]
fn a_view_of_a_bot_reaches_the_server_in_a_few_hundred_kilobytes_at_most() {
    // The largest a server can make it: a double chest and the inventory
    // full of items with 16 lore lines of 128 three-byte characters and 16
    // enchantments twice over, under the agent's message limit.
    let lore: Vec<String> = (0..16).map(|_| "\u{e000}".repeat(128)).collect();
    let enchantments: Vec<(String, i32)> = (0..16)
        .map(|n| (format!("minecraft:enchantment_{n}"), 255))
        .collect();
    let full = || {
        item("minecraft:potion", 64)
            .named(&"\u{e000}".repeat(64))
            .with_lore(&lore)
            .with_enchantments(enchantments.iter().map(|(id, level)| (id.as_str(), *level)))
            .with_stored_enchantments(enchantments.iter().map(|(id, level)| (id.as_str(), *level)))
    };
    let seen = Seen {
        inventory: BotInventory::new((0..46).map(|_| Some(full()))),
        screen: Some(Screen::shown(
            ScreenKind::Generic9x6,
            &"\u{e000}".repeat(128),
            (0..90).map(|_| Some(full())),
            Some(full()),
        )),
        sign: None,
    };

    let bytes = prost::Message::encoded_len(&wire::AgentMessage::try_from(answer(seen)).unwrap());

    assert!(
        bytes < fleet_proto::control::MAX_AGENT_MESSAGE_BYTES / 8,
        "{bytes} bytes"
    );
}
