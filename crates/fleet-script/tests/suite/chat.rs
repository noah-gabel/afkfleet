//! Chat in a script: declared messages out, declared patterns in (P15.31;
//! ADR-0036, D8; ADR-0037, C4, C6).

use core::time::Duration;

use fleet_core::script::{
    Captured, ChatMatch, Hook, HookEvent, Ident, MessageDraft, SendId, ValueDraft, ValueKind, Word,
};
use fleet_script::{Action, Instance, Misuse, Outcome, ScriptErrorKind, SourcePosition};
use rstest::rstest;

use crate::support::{World, balance_draft, logged, program_of, stopped, timer};

/// An instance of `source` under the balance mode's declarations, joined
/// in a fresh world.
fn joined(source: &str) -> (fleet_script::Ran, World, Instance) {
    let mut world = World::new();
    let mut instance = Instance::new(
        program_of(balance_draft(source)),
        7,
        crate::support::still(),
    );
    let ran = world.run(&mut instance, &HookEvent::Join { first: true });
    (ran, world, instance)
}

fn ident(text: &str) -> Ident {
    Ident::try_from(text).unwrap()
}

/// The sends among `actions`, as (number, key, text).
fn sends(actions: &[Action]) -> Vec<(u64, String, String)> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Send { id, key, message } => {
                Some((id.0, key.as_str().to_owned(), message.as_str().to_owned()))
            }
            _ => None,
        })
        .collect()
}

/// The windows asked for among `actions`, as (send, pattern, milliseconds).
fn expects(actions: &[Action]) -> Vec<(u64, String, u128)> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Expect {
                send,
                pattern,
                window,
            } => Some((send.0, pattern.as_str().to_owned(), window.as_millis())),
            _ => None,
        })
        .collect()
}

#[test]
fn a_declared_message_is_sent_by_its_key_from_a_hook_and_from_a_helper() {
    let (ran, world, _) = joined(
        "fn on_join(first) {\n  let a = chat.send(\"balance\");\n  ask();\n  log.info(`${a.ok} [${a.reason}]`);\n}\nfn ask() { chat.send(\"hello\"); }",
    );

    assert_eq!(
        sends(&ran.actions),
        [
            (0, "balance".to_owned(), "/balance".to_owned()),
            (1, "hello".to_owned(), "Hello there".to_owned())
        ]
    );
    assert_eq!(world.sent, ["/balance", "Hello there"]);
    assert_eq!(logged(&ran.actions), ["true []"]);
}

#[test]
fn a_message_with_values_takes_them_from_a_map() {
    let (ran, ..) = joined(
        "fn on_join(first) {\n  chat.send(\"pay\", #{ amount: 2500.5 });\n}\nfn on_timer(name) { chat.send(\"pay\", #{ amount: 70 }); }",
    );

    assert_eq!(
        sends(&ran.actions),
        [(0, "pay".to_owned(), "/pay AfkMain 2500.50".to_owned())]
    );
}

#[rstest]
#[case::out_of_range("chat.send(\"pay\", #{ amount: 0.5 })")]
#[case::too_large("chat.send(\"pay\", #{ amount: 1000000.01 })")]
#[case::not_a_number("chat.send(\"pay\", #{ amount: 0.0 / 0.0 })")]
#[case::a_text_for_a_number("chat.send(\"pay\", #{ amount: \"all\" })")]
#[case::a_missing_value("chat.send(\"pay\", #{})")]
#[case::no_values_at_all("chat.send(\"pay\")")]
#[case::a_value_nobody_declared("chat.send(\"pay\", #{ amount: 5, to: \"Other\" })")]
#[case::values_for_a_message_without("chat.send(\"hello\", #{ amount: 5 })")]
#[case::a_value_of_another_kind("chat.send(\"pay\", #{ amount: [1] })")]
#[case::values_that_are_no_map("chat.send(\"pay\", 5)")]
fn a_value_that_doesnt_fit_its_declaration_is_bad_value_and_nothing_is_sent(#[case] call: &str) {
    let (ran, world, _) = joined(&format!(
        "fn on_join(first) {{\n  let r = {call};\n  log.info(`${{r.ok}} [${{r.reason}}]`);\n}}"
    ));

    assert_eq!(logged(&ran.actions), ["false [bad_value]"], "{call}");
    assert_eq!(world.sent.len(), 0);
}

#[test]
fn a_word_value_takes_one_of_its_declared_words_only() {
    let mut draft = balance_draft(
        "fn on_join(first) {\n  let a = chat.send(\"warp\", #{ place: \"farm\" });\n  let b = chat.send(\"warp\", #{ place: \"spawn; /op me\" });\n  let c = chat.send(\"warp\", #{ place: 5 });\n  log.info(`${a.ok} ${b.reason} ${c.reason}`);\n}",
    );
    draft.messages.push(MessageDraft {
        key: "warp".to_owned(),
        text: "/warp {place}".to_owned(),
        values: vec![ValueDraft {
            name: "place".to_owned(),
            kind: ValueKind::Word {
                options: vec!["farm".to_owned(), "spawn".to_owned()],
            },
        }],
    });
    let mut world = World::new();
    let mut instance = Instance::new(program_of(draft), 7, crate::support::still());

    let ran = world.run(&mut instance, &HookEvent::Join { first: true });

    assert_eq!(world.sent, ["/warp farm"]);
    assert_eq!(logged(&ran.actions), ["true bad_value bad_value"]);
}

#[test]
fn an_unknown_key_stops_the_script_with_its_line() {
    for (call, column) in [
        ("chat.send(\"nope\")", 8),
        ("chat.send(\"nope\", #{ a: 1 })", 8),
        ("chat.send(5)", 8),
    ] {
        let (ran, world, _) = joined(&format!("fn on_join(first) {{\n  {call};\n}}"));

        let error = stopped(&ran);
        assert_eq!(
            error.kind,
            ScriptErrorKind::Misuse(Misuse::UnknownMessage),
            "{call}"
        );
        assert_eq!(error.hook, Hook::Join);
        assert_eq!(error.at, Some(SourcePosition { line: 2, column }));
        assert_eq!(world.sent.len(), 0);
    }
}

#[test]
fn plain_chat_waits_30_seconds_and_a_declared_command_doesnt() {
    let source = "fn on_join(first) {\n  let a = chat.send(\"hello\");\n  let b = chat.send(\"hello\");\n  let c = chat.send(\"balance\");\n  let d = chat.send(\"balance\");\n  let e = chat.send(\"pay\", #{ amount: 5 });\n  log.info(`${a.ok} ${b.reason} ${c.ok} ${d.ok} ${e.ok}`);\n}\nfn on_timer(name) { let r = chat.send(\"hello\"); log.info(`${r.ok}`); }";
    let (ran, mut world, mut instance) = joined(source);
    assert_eq!(logged(&ran.actions), ["true too_soon true true true"]);
    assert_eq!(
        world.sent,
        ["Hello there", "/balance", "/balance", "/pay AfkMain 5.00"]
    );

    world.now += Duration::from_millis(29_999);
    let early = world.run(&mut instance, &HookEvent::Timer(timer("t")));
    world.now += Duration::from_millis(1);
    let later = world.run(&mut instance, &HookEvent::Timer(timer("t")));

    assert_eq!(logged(&early.actions), ["false"]);
    assert_eq!(logged(&later.actions), ["true"]);
}

#[test]
fn expect_follows_a_declared_command_sent_earlier_in_the_same_hook() {
    let (ran, ..) = joined(
        "fn on_join(first) {\n  if chat.send(\"balance\").ok {\n    let r = chat.expect(\"balance\", 5000);\n    log.info(`${r.ok} [${r.reason}]`);\n  }\n}",
    );

    assert_eq!(
        sends(&ran.actions),
        [(0, "balance".to_owned(), "/balance".to_owned())]
    );
    assert_eq!(expects(&ran.actions), [(0, "balance".to_owned(), 5000)]);
    assert_eq!(logged(&ran.actions), ["true []"]);
}

#[test]
fn expect_works_from_a_helper_that_sent_the_command() {
    let (ran, ..) = joined(
        "fn on_join(first) { ask(); }\nfn ask() {\n  if chat.send(\"balance\").ok { listen(); }\n}\nfn listen() { chat.expect(\"balance\", 250); }",
    );

    assert_eq!(expects(&ran.actions), [(0, "balance".to_owned(), 250)]);
}

#[rstest]
#[case::nothing_sent("let r = chat.expect(\"balance\", 5000);")]
#[case::only_plain_chat_sent("chat.send(\"hello\");\n  let r = chat.expect(\"balance\", 5000);")]
#[case::the_command_was_refused(
    "chat.send(\"pay\", #{ amount: 0 });\n  let r = chat.expect(\"balance\", 5000);"
)]
#[case::the_unknown_command_was_caught(
    "try { chat.send(\"nope\"); } catch { }\n  let r = chat.expect(\"balance\", 5000);"
)]
fn expect_without_a_declared_command_sent_before_it_is_no_command(#[case] body: &str) {
    let (ran, ..) = joined(&format!(
        "fn on_join(first) {{\n  {body}\n  log.info(`${{r.ok}} [${{r.reason}}]`);\n}}"
    ));

    assert_eq!(logged(&ran.actions), ["false [no_command]"], "{body}");
    assert_eq!(expects(&ran.actions).len(), 0);
}

#[test]
fn a_command_of_an_earlier_hook_doesnt_count() {
    let source = "fn on_join(first) { chat.send(\"balance\"); }\nfn on_timer(name) {\n  let r = chat.expect(\"balance\", 5000);\n  log.info(`${r.ok} [${r.reason}]`);\n}\nfn on_chat(key, values) {\n  let r = chat.expect(\"balance\", 5000);\n  log.info(`${r.ok} [${r.reason}]`);\n}";
    let (_, mut world, mut instance) = joined(source);

    let timer_hook = world.run(&mut instance, &HookEvent::Timer(timer("t")));
    let chat_hook = world.run(
        &mut instance,
        &HookEvent::Chat(ChatMatch {
            key: ident("balance"),
            values: vec![(ident("amount"), Captured::Amount(5.0))],
        }),
    );

    assert_eq!(logged(&timer_hook.actions), ["false [no_command]"]);
    assert_eq!(logged(&chat_hook.actions), ["false [no_command]"]);
}

#[test]
fn a_command_refused_because_the_bot_is_dead_opens_no_window() {
    let source = "fn on_death() {\n  let s = chat.send(\"balance\");\n  let r = chat.expect(\"balance\", 5000);\n  log.info(`${s.reason} ${r.reason}`);\n}";
    let (_, mut world, mut instance) = joined(source);

    let ran = world.run(&mut instance, &HookEvent::Death);

    assert_eq!(logged(&ran.actions), ["dead dead"]);
    assert_eq!(expects(&ran.actions).len(), 0);
}

#[test]
fn expect_waits_for_the_last_command_sent_and_each_send_has_its_own_number() {
    let source = "fn on_join(first) {\n  chat.send(\"balance\");\n  chat.send(\"hello\");\n  chat.send(\"pay\", #{ amount: 5 });\n  chat.expect(\"balance\", 1000);\n}\nfn on_timer(name) {\n  chat.send(\"balance\");\n  chat.expect(\"balance\", 2000);\n}";
    let (ran, mut world, mut instance) = joined(source);

    let later = world.run(&mut instance, &HookEvent::Timer(timer("t")));

    assert_eq!(
        sends(&ran.actions)
            .iter()
            .map(|send| send.0)
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(expects(&ran.actions), [(2, "balance".to_owned(), 1000)]);
    assert_eq!(sends(&later.actions).first().map(|send| send.0), Some(3));
    assert_eq!(expects(&later.actions), [(3, "balance".to_owned(), 2000)]);
    assert_eq!(
        later.actions.first(),
        Some(&Action::Send {
            id: SendId(3),
            key: ident("balance"),
            message: fleet_core::chat::ChatMessage::try_from("/balance").unwrap(),
        })
    );
}

#[rstest]
#[case(249, false)]
#[case(250, true)]
#[case(30_000, true)]
#[case(30_001, false)]
#[case(0, false)]
#[case(-1, false)]
fn a_window_is_250_ms_to_30_seconds_long(#[case] ms: i64, #[case] ok: bool) {
    let (ran, ..) = joined(&format!(
        "fn on_join(first) {{\n  chat.send(\"balance\");\n  let r = chat.expect(\"balance\", {ms});\n  log.info(`${{r.ok}} [${{r.reason}}]`);\n}}"
    ));

    let expected = if ok { "true []" } else { "false [bad_value]" };
    assert_eq!(logged(&ran.actions), [expected]);
    assert_eq!(expects(&ran.actions).len(), usize::from(ok));
}

#[test]
fn expecting_a_pattern_the_mode_doesnt_declare_or_an_any_time_one_stops_the_script() {
    for (key, misuse) in [
        ("nope", Misuse::UnknownPattern),
        ("pay", Misuse::UnknownPattern),
        ("restart", Misuse::NotAReplyPattern),
    ] {
        let (ran, ..) = joined(&format!(
            "fn on_join(first) {{\n  chat.send(\"balance\");\n  chat.expect(\"{key}\", 5000);\n}}"
        ));

        let error = stopped(&ran);
        assert_eq!(error.kind, ScriptErrorKind::Misuse(misuse), "{key}");
        assert_eq!(error.at, Some(SourcePosition { line: 3, column: 8 }));
        assert_eq!(ran.actions, [Action::ReleaseAll]);
    }
}

#[test]
fn on_chat_gets_the_patterns_key_and_its_typed_values() {
    let source = "fn on_chat(key, values) {\n  log.info(`${key} ${type_of(values)} ${values.len()}`);\n  if key == \"balance\" { balance(values); } else { restart(values); }\n}\nfn balance(values) {\n  let amount = values.amount;\n  log.info(`${type_of(amount)} ${amount}`);\n}\nfn restart(values) {\n  let minutes = values.minutes;\n  let kind = values.kind;\n  log.info(`${type_of(minutes)} ${minutes} ${type_of(kind)} ${kind}`);\n}";
    let (_, mut world, mut instance) = joined(source);

    world.run(
        &mut instance,
        &HookEvent::Chat(ChatMatch {
            key: ident("balance"),
            values: vec![(ident("amount"), Captured::Amount(12_500.5))],
        }),
    );
    world.run(
        &mut instance,
        &HookEvent::Chat(ChatMatch {
            key: ident("restart"),
            values: vec![
                (ident("minutes"), Captured::Integer(5)),
                (
                    ident("kind"),
                    Captured::Word(Word::try_from("daily").unwrap()),
                ),
            ],
        }),
    );

    assert_eq!(
        world.log,
        [
            "balance map 1",
            "f64 12500.5",
            "restart map 2",
            "i64 5 string daily",
        ]
    );
}

#[test]
fn a_value_from_chat_fills_a_declared_value_inside_its_range_and_nothing_else() {
    let source = "fn on_chat(key, values) {\n  let over = values.amount - 10000;\n  if over >= 1 {\n    let r = chat.send(\"pay\", #{ amount: over });\n    log.info(`${r.ok} [${r.reason}]`);\n  }\n}";
    let reply = |amount: f64| {
        let (_, mut world, mut instance) = joined(source);
        world.run(
            &mut instance,
            &HookEvent::Chat(ChatMatch {
                key: ident("balance"),
                values: vec![(ident("amount"), Captured::Amount(amount))],
            }),
        );
        (world.sent, world.log)
    };

    assert_eq!(
        reply(12_500.5),
        (
            vec!["/pay AfkMain 2500.50".to_owned()],
            vec!["true []".to_owned()]
        )
    );
    assert_eq!(reply(10_000.5), (vec![], vec![]));
    // The declared range ends at 1,000,000: more than that is never sent.
    assert_eq!(
        reply(900_000_000.0),
        (vec![], vec!["false [bad_value]".to_owned()])
    );
}

#[test]
fn a_script_without_on_chat_ignores_a_match() {
    let (_, mut world, mut instance) = joined("fn on_join(first) { }");

    let ran = world.run(
        &mut instance,
        &HookEvent::Chat(ChatMatch {
            key: ident("balance"),
            values: Vec::new(),
        }),
    );

    assert_eq!(ran.outcome, Outcome::NotDefined);
    assert_eq!(ran.actions.len(), 0);
}

#[test]
fn on_stop_sends_one_declared_message_and_acts_on_nothing_else() {
    let source = "fn on_join(first) { keys.forward.hold(); }\nfn on_stop() {\n  let j = keys.jump.tap();\n  let a = chat.send(\"balance\");\n  let e = chat.expect(\"balance\", 5000);\n  let b = chat.send(\"balance\");\n  let t = timer.after(1000, \"t\");\n  let r = keys.forward.release();\n  log.info(`${j.reason} ${a.ok} ${e.reason} ${b.reason} ${t.reason} ${r.ok}`);\n}";
    let (_, mut world, mut instance) = joined(source);

    let ran = world.run(&mut instance, &HookEvent::Stop);

    assert_eq!(ran.outcome, Outcome::Ended { error: None });
    assert_eq!(
        logged(&ran.actions),
        ["not_in_world true not_in_world not_in_world not_in_world true"]
    );
    assert_eq!(world.sent.last().map(String::as_str), Some("/balance"));
    assert_eq!(sends(&ran.actions).len(), 1);
    assert_eq!(ran.actions.last(), Some(&Action::ReleaseAll));
    assert_eq!(ran.next_wake, None);
}
