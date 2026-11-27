//! Each target's seeds: a few inputs that already reach what the target
//! is about (a valid value, a whole message, a sealed envelope's shape), so
//! a run starts there, not at the empty input.
//!
//! They're built here, from the real types, so they can't go stale in a
//! folder of binary files: this crate's tests run every target over them,
//! and its `seeds` example writes them where cargo-fuzz reads a corpus.

use core::time::Duration;

use fleet_core::id::{AgentId, BotId};
use fleet_core::value::{AgentCapacity, AgentName, AgentVersion};
use fleet_proto::PROTOCOL_VERSION;
use fleet_proto::control::{AgentMessage, Hello, ServerMessage, Welcome};
use fleet_proto::generated as wire;
use fleet_server::ports::vault::header_of;
use prost::Message as _;
use uuid::Uuid;

/// An ID's UUID: version 7, as every ID is.
const ID: Uuid = Uuid::from_u128(0x0189_0000_0000_7000_8000_0000_0000_0001);

/// The seeds of the target `name`; none for a name that isn't a target's.
#[must_use]
pub fn seeds(name: &str) -> Vec<Vec<u8>> {
    if let Some(menus) = crate::menu::seeds(name) {
        return menus;
    }
    match name {
        "value_objects" => texts(&[
            "alice",
            "AfkBot1",
            "mc.example.com:25565",
            "[2001:db8::1]:25565",
            "192.0.2.0/24",
            "01890000-0000-7000-8000-000000000001",
            "agent-1",
            "0.1.2",
            "owner",
            "manage",
            "shared",
            "bot.start",
            "/spawn\n/home",
        ]),
        "chat_sanitizer" => texts(&[
            "\u{40}\0§aHello\r\nworld",
            "\u{8}\0a\u{202e}b\u{200b}c\nd",
            "\0\u{8}§",
        ]),
        "chat_formats" => texts(&[
            "[{any}] {sender} : \n{sender} : ",
            "{{{sender}}} » ",
            "{sender",
            "",
        ]),
        "chat_sender" => texts(&[
            "[{any}] {sender} : \n{sender} : \0[VIP] Steve : hello",
            "{sender} : \0Steve : hello",
            "<{sender}> \0<Alex> hi\nthere",
        ]),
        "private_targets" => texts(&[
            "10.0.0.0/8\n192.168.1.0/24",
            "fd00::/8\n::1/128",
            "0.0.0.0/0",
        ]),
        "mode_definition" => texts(&[
            r#"{"steps":[{"action":{"type":"swing_arm"},"schedule":{"type":"every","interval_ms":20000,"jitter_ms":0},"probability":100}]}"#,
            r#"{"name":"Farm","visibility":"private","steps":[{"action":{"type":"send_chat","message":"/spawn"},"schedule":{"type":"at_start"},"probability":100}]}"#,
            r#"{"version":1,"name":"Farm","visibility":"shared","steps":[{"action":{"type":"look","yaw":-90.5,"pitch":12.0},"schedule":{"type":"at_start"},"probability":50}]}"#,
            // A scripted mode with every kind of declaration, and the
            // smallest one (Phase 15).
            r#"{"script":{"api":1,"source":"fn on_join(first) {\n    chat.send(\"balance\");\n}\n","leash":8,"tool_guard":2,"block_guard":[],"messages":[{"key":"balance","text":"/balance","values":[]},{"key":"pay","text":"/pay {to} {amount} x{n}","values":[{"name":"to","type":"word","options":["AfkMain","AfkBot2"]},{"name":"amount","type":"decimal","min":0.01,"max":1000000.0,"places":2},{"name":"n","type":"integer","min":1,"max":64}]}],"patterns":[{"key":"balance","when":"reply","text":"Balance: ${amount}","captures":[{"name":"amount","type":"amount","min":0,"max":1000000000,"decimal_mark":"."}]},{"key":"shop","when":"any_time","text":"Shop is {state} with {n} items","captures":[{"name":"state","type":"word","options":["open","closed"]},{"name":"n","type":"integer","min":0,"max":64}]}]}}"#,
            r#"{"script":{"api":1,"source":"","leash":64,"tool_guard":0,"block_guard":[],"messages":[],"patterns":[]}}"#,
        ]),
        "agent_message" => agent_messages(),
        "server_message" => server_messages(),
        "vault_envelope" => {
            let mut shaped = header_of(1).to_vec();
            shaped.extend_from_slice(&[0; 48]);
            vec![b"\x07\x01a secret".to_vec(), shaped, vec![1, 0, 0, 0, 1]]
        }
        "agent_metrics" => texts(&[
            "GET /metrics HTTP/1.1\r\nHost: agent\r\n\r\n",
            "HEAD /metrics?x=1 HTTP/1.0\r\n\r\n",
            "POST /metrics HTTP/1.1\r\n\r\n",
            "GET / HTTP/1.1\r\n\r\n",
            "GET /metrics HTTP/1.1\r\n",
        ]),
        _ => scripted(name),
    }
}

/// The seeds of the scripted modes' targets (Phase 15); none for another
/// name.
fn scripted(name: &str) -> Vec<Vec<u8>> {
    match name {
        "script_compile" => scripts()
            .into_iter()
            .map(str::as_bytes)
            .chain([
                // What validation refuses, each for its own reason.
                &b"fn on_join(first) {\n  let b = ;\n}"[..],
                b"let x = 1;\nfn on_join(first) { }",
                b"fn on_join(first) { let keys = 1; }",
                b"fn on_join(first) { kees.jump.tap(); }",
                b"fn on_join() { }\nfn on_reconnect() { }",
                b"fn on_join(first) { let f = |x| x; eval(\"1\"); import \"a\" as b; }",
                b"fn f(state) { for bot in [1] { } try { } catch (world) { } }",
                LONG_HOOK_NAME.as_bytes(),
                RAW_STRING.as_bytes(),
                b"fn f() { `a ${ `b ${ \"c\" }` }` + 'x' /* /* nested */ */ }",
            ])
            .map(<[u8]>::to_vec)
            .collect(),
        "script_run" => scripts()
            .into_iter()
            .chain([BUSY_SCRIPT, GREEDY_SCRIPT])
            .map(|script| {
                let mut seed = script.as_bytes().to_vec();
                seed.push(0);
                // A first join, then every event in turn with pauses from
                // none to minutes, and a reconnect.
                seed.extend_from_slice(&[
                    0, 1, 0, 16, 3, 4, 18, 7, 5, 2, 9, 1, 30, 4, 2, 9, 4, 3, 250, 22, 4, 0, 1, 0,
                    8, 13, 5, 4, 2, 4, 12, 8, 4, 40, 1, 6,
                ]);
                seed
            })
            .collect(),
        "script_api" => {
            // Every member in turn, with one to four arguments that move
            // through the whole list; then the facts' shape.
            let mut calls: Vec<Vec<u8>> = (0..4_u8)
                .map(|round| {
                    let mut seed: Vec<u8> = (0..40_u8)
                        .flat_map(|pick| {
                            let member = pick.wrapping_add(round.wrapping_mul(40));
                            let first = pick.wrapping_mul(7).wrapping_add(round);
                            [
                                member,
                                round + 1,
                                first,
                                first.wrapping_add(11),
                                first.wrapping_add(19),
                                first.wrapping_add(23),
                            ]
                        })
                        .collect();
                    seed.extend_from_slice(&[0, 6 + round]);
                    seed
                })
                .collect();
            calls.push(vec![0, 0]);
            calls
        }
        "message_template" => texts(&[
            "{\"key\":\"pay\",\"text\":\"/pay AfkMain {amount}\",\"values\":[{\"name\":\"amount\",\"type\":\"decimal\",\"min\":1.0,\"max\":1000000.0,\"places\":2}]}\0amount=2500.5",
            "{\"key\":\"sell\",\"text\":\"/ah sell {price} x{n}\",\"values\":[{\"name\":\"price\",\"type\":\"integer\",\"min\":1,\"max\":1000000},{\"name\":\"n\",\"type\":\"integer\",\"min\":1,\"max\":64}]}\0price=1500\nn=64",
            "{\"key\":\"warp\",\"text\":\"/warp {place}\",\"values\":[{\"name\":\"place\",\"type\":\"word\",\"options\":[\"farm\",\"spawn\"]}]}\0place=farm",
            "{\"key\":\"hello\",\"text\":\"Hello {{there}}\",\"values\":[]}\0",
            "{\"key\":\"pay\",\"text\":\"/pay AfkMain {amount}\",\"values\":[{\"name\":\"amount\",\"type\":\"decimal\",\"min\":1.0,\"max\":1000000.0,\"places\":2}]}\0amount=NaN\nto=Other",
        ]),
        "chat_pattern" => texts(&[
            "{\"key\":\"balance\",\"when\":\"reply\",\"text\":\"Balance: ${amount}\",\"captures\":[{\"name\":\"amount\",\"type\":\"amount\",\"min\":0,\"max\":1000000000,\"decimal_mark\":\".\"}]}\0Balance: $12,500.50",
            "{\"key\":\"balance\",\"when\":\"reply\",\"text\":\"Balance: ${amount}\",\"captures\":[{\"name\":\"amount\",\"type\":\"amount\",\"min\":0,\"max\":1000000000,\"decimal_mark\":\".\"}]}\0<Steve> Balance: $3M",
            "{\"key\":\"shop\",\"when\":\"any_time\",\"text\":\"Shop is {state} with {n} items\",\"captures\":[{\"name\":\"state\",\"type\":\"word\",\"options\":[\"open\",\"closed\"]},{\"name\":\"n\",\"type\":\"integer\",\"min\":0,\"max\":64}]}\0  Shop is closed with 64 items \n",
            "{\"key\":\"euro\",\"when\":\"reply\",\"text\":\"Kontostand: {amount} Taler\",\"captures\":[{\"name\":\"amount\",\"type\":\"amount\",\"min\":-1000,\"max\":1000,\"decimal_mark\":\",\"}]}\0Kontostand: -1.000,0 Taler",
        ]),
        _ => Vec::new(),
    }
}

/// The reference's examples: scripts that compile and act.
fn scripts() -> Vec<&'static str> {
    // The menu examples are level 2's: the menus' targets read their
    // screens.
    fleet_script::EXAMPLES
        .iter()
        .filter(|example| example.level == fleet_core::script::ScriptApiLevel::V1)
        .map(|example| example.source)
        .collect()
}

/// A function named like a hook that is none, its name 246 characters
/// long: the input the fuzzer found on CI on 2026-11-17, byte for byte,
/// where the error carried the whole name and its line grew past its bound.
const LONG_HOOK_NAME: &str = "fn on_inconnecton_recojoimmmmmmmmmmmmmtmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmemmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmemmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmnconnecton_reconnect() { }";

/// A raw string with a quote right before a line break, then a mistake:
/// Rhai 1.27.0 loses a line there, so the mistake's place was a line off,
/// past the end of an empty line. The fuzzer found such an input on CI on
/// 2026-11-21 (column 41 of an empty line); a raw string is now refused at
/// its own place (ADR-0040's amendment, A6).
const RAW_STRING: &str = "fn f() {\n    let s = #\"a\"\nb\"#;\n\n    let t = ) ;\n}";

/// A script that does something of everything in every hook, picked by
/// its own random numbers.
const BUSY_SCRIPT: &str = r#"fn on_join(first) { act(); }
fn on_world_change() { act(); }
fn on_death() { act(); }
fn on_respawn() { act(); }
fn on_timer(name) { act(); }
fn on_chat(key, values) { act(); pay(values); }
fn on_stop() { chat.send("balance"); keys.forward.hold(); }

fn act() {
    if !("n" in state) { state.n = 0; }
    state.n += 1;
    press(random.between(0, 14));
    let ms = random.between(250, 3000);
    let which = random.between(0, 20);
    if random.chance(40) { timer.after(ms, `t${which}`); }
    if random.chance(20) { timer.every(ms, "again"); }
    if random.chance(15) { mouse.left.release(); mouse.right.release(); }
    if random.chance(20) { ask(); }
    log.info(`${state.n} ${bot.riding} ${world.nearest_free("minecart") == ()}`);
}

fn ask() {
    if chat.send("balance").ok { chat.expect("balance", 1000); }
}

fn pay(values) {
    chat.send("pay", #{ amount: values.amount - 1000 });
}

fn press(pick) {
    let a = random.between(-180, 180);
    let b = random.between(-90, 90);
    let ms = random.between(50, 2000);
    let slot = random.between(1, 9);
    switch pick {
        0 => keys.forward.hold(),
        1 => keys.forward.tap(),
        2 => keys.jump.hold_for(ms),
        3 => mouse.left.tap(),
        4 => mouse.left.hold(),
        5 => mouse.right.tap(),
        6 => mouse.right.hold(),
        7 => look.at(a, b),
        8 => look.turn(1, 1),
        9 => hotbar.select(slot),
        10 => vehicle.mount("minecart"),
        11 => vehicle.dismount(),
        12 => keys.release_all(),
        13 => mouse.right.hold_until_used(),
        _ => keys.sneak.hold_for(60),
    }
}
"#;

/// A script that wants more than it may have: every hook holds a key and
/// then fills the state, loops for ever or calls itself for ever.
const GREEDY_SCRIPT: &str = r#"fn on_join(first) { keys.sprint.hold(); grow(); }
fn on_world_change() { mouse.left.hold(); loop { } }
fn on_death() { down(1); }
fn on_respawn() { keys.jump.hold_for(500); state = 5; }
fn on_timer(name) { state.f = Fn("grow"); }
fn on_chat(key, values) { let s = "x"; loop { s += s; } }
fn on_stop() { loop { } }

fn grow() {
    let i = 0;
    loop { state[`k${i}`] = state; i += 1; }
}

fn down(n) { down(n + 1) }
"#;

/// `texts` as byte strings.
fn texts(texts: &[&str]) -> Vec<Vec<u8>> {
    texts.iter().map(|text| text.as_bytes().to_vec()).collect()
}

/// A hello, a heartbeat and an enrollment request, as an agent sends them.
fn agent_messages() -> Vec<Vec<u8>> {
    let hello = AgentName::try_from("agent-1")
        .ok()
        .zip(AgentVersion::try_from("0.1.2").ok())
        .zip(AgentCapacity::try_from(20).ok())
        .map(|((name, agent_version), capacity)| {
            AgentMessage::Hello(Hello {
                name,
                agent_version,
                protocol_version: PROTOCOL_VERSION,
                capacity,
                running: Vec::new(),
                script_api: 1,
            })
        });
    let scripts = script_reports();
    let enroll = wire::EnrollRequest {
        token: "a-token".to_owned(),
        name: "agent-1".to_owned(),
        csr_der: vec![0x30, 0x00],
    };
    hello
        .into_iter()
        .chain([AgentMessage::Heartbeat])
        .chain(scripts)
        .filter_map(|message| wire::AgentMessage::try_from(message).ok())
        .map(|message| message.encode_to_vec())
        .chain([enroll.encode_to_vec()])
        .collect()
}

/// A bot's script's status with its problem's place, and a line of its
/// log, as an agent reports them (Phase 15).
fn script_reports() -> Vec<AgentMessage> {
    use fleet_core::script::{
        Hook, LogKind, ProblemKind, ReportText, ScriptLogLine, ScriptPlace, ScriptProblem,
        ScriptState,
    };
    use fleet_proto::control::{BotEvent, BotEventKind};

    let Ok(bot_id) = BotId::try_from(ID) else {
        return Vec::new();
    };
    let at = chrono::DateTime::UNIX_EPOCH;
    let event = |kind| AgentMessage::Event(BotEvent { bot_id, at, kind });
    vec![
        event(BotEventKind::ScriptStatus(ScriptState::Stopped(
            ScriptProblem {
                kind: ProblemKind::Error,
                hook: Some(Hook::Chat),
                place: ScriptPlace::new(3, 7),
                message: ReportText::clean("Function not found: pay"),
            },
        ))),
        event(BotEventKind::ScriptLog(ScriptLogLine {
            kind: LogKind::Sent,
            text: ReportText::clean("sent `balance`"),
        })),
    ]
}

/// The scripted mode with every kind of declaration, as a mode stores it.
const SCRIPTED: &str = r#"{"script":{"api":1,"source":"fn on_join(first) {\n    chat.send(\"balance\");\n}\n","leash":8,"tool_guard":2,"block_guard":["minecraft:cobblestone"],"messages":[{"key":"balance","text":"/balance","values":[]},{"key":"pay","text":"/pay {to} {amount} x{n}","values":[{"name":"to","type":"word","options":["AfkMain","AfkBot2"]},{"name":"amount","type":"decimal","min":0.01,"max":1000000.0,"places":2},{"name":"n","type":"integer","min":1,"max":64}]}],"patterns":[{"key":"balance","when":"reply","text":"Balance: ${amount}","captures":[{"name":"amount","type":"amount","min":0,"max":1000000000,"decimal_mark":"."}]}]}}"#;

/// A bot's assignment to the scripted mode, with a server's chat formats,
/// as the server sends one (Phase 15).
fn scripted_assignment() -> Option<ServerMessage> {
    use fleet_core::bot::{BotAccount, BotSpec, DesiredRunState};
    use fleet_core::chat::{ChatFormats, ChatRate};
    use fleet_core::disconnect::ConflictTexts;
    use fleet_proto::control::Assignment;

    Some(ServerMessage::Assign(Assignment {
        spec: BotSpec {
            id: BotId::try_from(ID).ok()?,
            account: BotAccount::Offline("AfkBot1".parse().ok()?),
            server: "mc.example.com".try_into().ok()?,
            mode: serde_json::from_str(SCRIPTED).ok()?,
            desired: DesiredRunState::Running,
            conflict_texts: ConflictTexts::default(),
            chat_rate: ChatRate::default(),
            chat_formats: ChatFormats::try_new(&["[{any}] {sender} : ", "<{sender}> "]).ok()?,
        },
        restore: None,
    }))
}

/// A welcome, an unassignment and a scripted bot's assignment, as the
/// server sends them.
fn server_messages() -> Vec<Vec<u8>> {
    let welcome = AgentId::try_from(ID).ok().map(|agent_id| {
        ServerMessage::Welcome(Welcome {
            heartbeat_interval: Duration::from_secs(10),
            agent_id,
        })
    });
    let unassign = BotId::try_from(ID).ok().map(ServerMessage::Unassign);
    welcome
        .into_iter()
        .chain(unassign)
        .chain(scripted_assignment())
        .filter_map(|message| wire::ServerMessage::try_from(message).ok())
        .map(|message| message.encode_to_vec())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TARGETS;

    #[test]
    fn every_target_has_seeds_and_no_other_name_has() {
        for (name, _) in TARGETS {
            assert!(!seeds(name).is_empty(), "{name}");
        }
        assert_eq!(seeds("no_such_target"), Vec::<Vec<u8>>::new());
    }

    /// The scripted mode a seed's text is, under the balance example's
    /// declarations, compiled.
    fn compiled(source: &[u8]) -> Result<fleet_script::Program, String> {
        let declared = fleet_script::EXAMPLES
            .iter()
            .find(|example| example.id == "balance")
            .unwrap();
        let script = fleet_core::script::Script::try_from(fleet_core::script::ScriptDraft {
            source: String::from_utf8(source.to_vec()).unwrap(),
            ..declared.draft()
        })
        .map_err(|error| error.to_string())?;
        fleet_script::compile(&script).map_err(|error| error.to_string())
    }

    // A seed that didn't compile would pass every property of its target
    // without running a line.
    #[test]
    fn the_script_seeds_that_are_meant_to_run_compile() {
        for seed in seeds("script_run") {
            let (source, steps) = crate::halves(&seed);

            assert!(compiled(source).is_ok(), "{:?}", compiled(source).err());
            assert!(steps.len() >= 30);
        }
        assert_eq!(seeds("script_run").len(), 6);
    }

    #[test]
    fn the_compile_seeds_are_the_examples_and_one_script_for_each_refusal() {
        let seeds = seeds("script_compile");
        let (examples, refused) = seeds.split_at(4);

        for seed in examples {
            assert!(compiled(seed).is_ok(), "{:?}", compiled(seed).err());
        }
        // The last is no refusal: texts in texts and comments in
        // comments, for the scan that finds an error's place.
        let (scanned, refused) = refused.split_last().unwrap();
        assert!(compiled(scanned).is_ok());
        let reasons: Vec<String> = refused
            .iter()
            .map(|seed| compiled(seed).unwrap_err())
            .collect();
        assert_eq!(reasons.len(), 9);
        // Each for a reason of its own.
        let mut distinct = reasons.clone();
        distinct.sort();
        distinct.dedup();
        assert!(distinct.len() >= 7, "{reasons:?}");
    }

    #[test]
    fn the_long_hook_name_is_the_input_the_fuzzer_found() {
        let input = seeds("script_compile")
            .into_iter()
            .find(|seed| seed.starts_with(b"fn on_inconnecton_"))
            .unwrap();
        let name = input
            .strip_prefix(b"fn ")
            .and_then(|rest| rest.strip_suffix(b"() { }"))
            .unwrap();

        assert_eq!((input.len(), name.len()), (255, 246));
        assert!(name.len() > fleet_script::CompileError::MAX_MESSAGE_CHARS);
        assert!(compiled(&input).unwrap_err().contains("isn't a hook"));
    }

    #[test]
    fn the_declaration_seeds_are_declarations_fleet_core_takes() {
        use fleet_core::script::{ChatPattern, MessageDraft, MessageTemplate, PatternDraft};

        for seed in seeds("message_template") {
            let (declaration, _) = crate::halves(&seed);
            let draft: MessageDraft = serde_json::from_slice(declaration).unwrap();

            assert!(MessageTemplate::try_from(draft).is_ok());
        }
        let fits: Vec<bool> = seeds("chat_pattern")
            .iter()
            .map(|seed| {
                let (declaration, line) = crate::halves(seed);
                let draft: PatternDraft = serde_json::from_slice(declaration).unwrap();
                let pattern = ChatPattern::try_from(draft).unwrap();
                pattern.matches(&String::from_utf8_lossy(line)).is_some()
            })
            .collect();
        // A reply, the same line as a player's, a line with spaces around
        // it, and another decimal mark at its range's very end.
        assert_eq!(fits, [true, false, true, true]);
    }

    #[test]
    fn the_message_seeds_are_whole_messages() {
        // A hello, a heartbeat, a script's status and a line of its log, and
        // an enrollment request.
        assert_eq!(agent_messages().len(), 5);
        // A welcome, an unassignment and a scripted bot's assignment.
        let server = server_messages();
        assert_eq!(server.len(), 3);
        let assigned = wire::ServerMessage::decode(server[2].as_slice()).unwrap();
        let Ok(ServerMessage::Assign(assignment)) = ServerMessage::try_from(assigned) else {
            panic!("an assignment");
        };
        assert!(assignment.spec.mode.script().is_some());
        assert_eq!(assignment.spec.chat_formats.iter().count(), 2);
    }
}
