//! The long run's ledger (Plan.md P16.18; ADR-0054): what a buyer's clicks
//! did, counted from the Paper harness's own records against what the
//! script saw and said, never from the bot's word alone.
//!
//! - **What the script saw** is the runner's line for every screen command
//!   it hands the session ([`SCREEN_COMMAND`]): the click's window, the
//!   state the hook saw, the slot and the item there, lore and all. The
//!   session sends it only onto that screen in that state; a command it
//!   refused is followed by the script's `Menu { …, sent: false }` entry.
//! - **What the script said** is its log: the runner's entries, its own
//!   lines (`Missed <price> from <seller>: <why>.`) and the click lines.
//! - **What happened** is the harness's: every click it received, with
//!   the window and the state its packet carried as the server read it off
//!   the network, the state the server had when it handled it, and the
//!   listing it met; every packet no handler got, its window gone; and
//!   every purchase.
//!
//! The counts the definition of done names:
//! - **Double buys:** a purchase of the bot's that no pick of its meant (a
//!   pick is a click on a listing the harness received; the listing it
//!   meant is the one the script saw there, told apart by its price, which
//!   the run keeps unique), or a second purchase for one pick.
//! - **Silent misses:** a pick that bought nothing, without a line of the
//!   script's naming its price.
//! - **Clicks onto another screen:** a click the harness received whose
//!   packet carried another window or state than the script saw, or one the
//!   script never asked for.
//!
//! Beside them: a click sent onto the state the script saw that met a newer
//! one, the server having redrawn the screen while it was on its way, which
//! no client can prevent (the server applies it anyway, then sends the
//! whole screen); the script's next screen judges what it did.

use std::collections::BTreeMap;

use serde_json::Value;

use super::paper::Record;

/// The runner's line for every screen command it hands the session.
pub(crate) const SCREEN_COMMAND: &str = "the script does something in a screen or a sign editor";
/// The runner's line for an entry of a script's log.
const LOGGED: &str = "the bot's script logged";
/// The auction house's title, whose slots 0 to 44 are listings.
const HOUSE: &str = "Auction House";

/// A click the runner handed the session, as the hook saw it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Asked {
    pub(crate) window: u32,
    pub(crate) state: u32,
    pub(crate) slot: u32,
    /// The price on the item the hook saw in the slot, if its lore has one.
    pub(crate) price: Option<f64>,
    /// Whether the session sent it.
    pub(crate) sent: bool,
}

/// What the long run counted; see the [module docs](self).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Trades {
    /// Clicks the runner handed the session, sent or not.
    pub(crate) asked: usize,
    /// Of those, the ones the session sent.
    pub(crate) sent: usize,
    /// Clicks the harness received from the bot.
    pub(crate) received: usize,
    /// Each fate of a sent click: ok, rejected, unclear, replaced,
    /// unanswered.
    pub(crate) fates: BTreeMap<String, usize>,
    /// The session's reasons for the clicks it didn't send.
    pub(crate) not_sent: BTreeMap<String, usize>,
    /// Clicks on a listing the harness received: the buys the script meant.
    pub(crate) picks: usize,
    /// The bot's purchases.
    pub(crate) bought: usize,
    /// Every miss line of the script's, as written.
    pub(crate) misses: Vec<String>,
    /// Purchases no pick meant, or a second for one.
    pub(crate) double_buys: Vec<String>,
    /// Picks that bought nothing without a line naming their price.
    pub(crate) silent_misses: Vec<String>,
    /// Clicks whose packet carried another window or state than the
    /// script saw, or never asked for.
    pub(crate) other_screens: Vec<String>,
    /// Clicks sent onto the state the script saw that met a newer one: the
    /// server redrew the screen while they were on their way.
    pub(crate) changed_in_flight: Vec<String>,
    /// Click packets of the bot's no handler got: their window was gone.
    pub(crate) ignored: usize,
    /// Picks whose click met another listing than the script saw there.
    pub(crate) other_listings: Vec<String>,
    /// Miss lines for a listing the bot did buy (the script's word wrong,
    /// the other way).
    pub(crate) missed_but_bought: Vec<String>,
    /// The fates the script heard, as its `Answered N clicks.` line at its
    /// stop says.
    pub(crate) answered: Option<usize>,
    /// Sent clicks that got no fate: their session ended first.
    pub(crate) unheard: usize,
}

/// The number after `marker` in `text`, its digits only.
fn number_after(text: &str, marker: &str) -> Option<u32> {
    let rest = text.get(text.find(marker)? + marker.len()..)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The price in a lore line `Price: $1,234.56` within `text`.
fn price_in(text: &str) -> Option<f64> {
    let rest = text.get(text.find("Price: $")? + "Price: $".len()..)?;
    let number: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.')
        .filter(|c| *c != ',')
        .collect();
    number.trim_end_matches('.').parse().ok()
}

/// A click command's line: `Act(Click(Click { seen: Seen { screen:
/// ScreenId { window: WindowId(3), … }, state: StateId(7), … }, kind: …,
/// slot: 13, touched: [(13, Some(Item { … lore … }))] }))`.
pub(crate) fn asked(command: &str) -> Option<Asked> {
    if !command.starts_with("Act(Click(") {
        return None;
    }
    let after_kind = command.get(command.find(", kind: ")?..)?;
    Some(Asked {
        window: number_after(command, "window: WindowId(")?,
        state: number_after(command, "state: StateId(")?,
        slot: number_after(after_kind, ", slot: ")?,
        price: price_in(command),
        sent: true,
    })
}

/// A script's log entry, as the runner logs it in Rust's debug form.
#[derive(Debug, Clone, PartialEq)]
enum Entry {
    /// A click's fate other than ok, or a click the session didn't send.
    Click { reason: String, sent: bool },
    /// A line of the script's own.
    Line(String),
    /// Anything else.
    Other,
}

fn entry(text: &str) -> Entry {
    if text.starts_with("Menu { asked: Click") {
        let reason = text
            .split_once("reason: ")
            .and_then(|(_, rest)| rest.split(',').next())
            .unwrap_or_default();
        return Entry::Click {
            reason: snake(reason),
            sent: text.contains("sent: true"),
        };
    }
    if let Some(line) = text
        .strip_prefix("Line(\"")
        .and_then(|rest| rest.strip_suffix("\")"))
    {
        return Entry::Line(line.replace("\\\"", "\""));
    }
    Entry::Other
}

/// `ScreenChanged` as the script reads it: `screen_changed`.
fn snake(name: &str) -> String {
    let mut out = String::new();
    for (at, c) in name.trim().chars().enumerate() {
        if c.is_ascii_uppercase() {
            if at > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The price a miss line names: `Missed 501.37 from …`.
fn missed_price(line: &str) -> Option<f64> {
    line.strip_prefix("Missed ")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn same_price(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.005
}

/// Counts `harness`, the Paper harness's records, against `agent`, the
/// agent's log lines in order, for the bot `name`; see the [module
/// docs](self).
pub(crate) fn count(harness: &[Record], agent: &[Value], name: &str) -> Trades {
    let mut trades = Trades::default();
    let asks = read_the_agent(agent, &mut trades);
    let mut picks = match_the_clicks(harness, &asks, name, &mut trades);
    take_the_purchases(harness, name, &mut picks, &mut trades);
    name_the_misses(&picks, &mut trades);
    trades
}

/// What the script asked, its fates, the clicks the session didn't send
/// and its miss lines, in order.
fn read_the_agent(agent: &[Value], trades: &mut Trades) -> Vec<Asked> {
    let mut asks: Vec<Asked> = Vec::new();
    for line in agent {
        let message = line.get("message").and_then(Value::as_str);
        if message == Some(SCREEN_COMMAND) {
            if let Some(ask) = line.get("command").and_then(Value::as_str).and_then(asked) {
                asks.push(ask);
            }
        } else if message == Some(LOGGED) {
            match entry(
                line.get("entry")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ) {
                Entry::Click { reason, sent: true } => {
                    *trades.fates.entry(reason).or_default() += 1;
                }
                Entry::Click {
                    reason,
                    sent: false,
                } => {
                    // The command the session just refused.
                    if let Some(last) = asks.last_mut() {
                        last.sent = false;
                    }
                    *trades.not_sent.entry(reason).or_default() += 1;
                }
                Entry::Line(text) if text.starts_with("Missed ") => trades.misses.push(text),
                Entry::Line(text) if text.starts_with("Answered ") => {
                    trades.answered = text
                        .split_whitespace()
                        .nth(1)
                        .and_then(|count| count.parse().ok());
                }
                Entry::Line(_) | Entry::Other => {}
            }
        }
    }
    trades.asked = asks.len();
    trades.sent = asks.iter().filter(|ask| ask.sent).count();
    // A fate other than ok is a line; ok is the rest of what the script
    // heard. A sent click whose session ended first got no fate at all.
    let not_ok: usize = trades.fates.values().sum();
    let heard = trades.answered.unwrap_or(trades.sent);
    trades
        .fates
        .insert("ok".to_owned(), heard.saturating_sub(not_ok));
    trades.unheard = trades.sent.saturating_sub(heard);
    asks
}

/// Matches each click the harness received to the first sent command after
/// the last one matched with its window and slot: the harness hears a
/// subsequence of what was sent (a click for a window the server has closed
/// never reaches a handler). Returns the picks: the price the script saw,
/// and whether a purchase took it yet.
fn match_the_clicks(
    harness: &[Record],
    asks: &[Asked],
    name: &str,
    trades: &mut Trades,
) -> Vec<(f64, bool)> {
    let prices: BTreeMap<String, f64> = harness
        .iter()
        .filter(|record| record.kind == "listed")
        .filter_map(|record| {
            Some((
                record.get("id")?.to_owned(),
                record.get("price")?.parse().ok()?,
            ))
        })
        .collect();
    let mut next = 0;
    let mut picks = Vec::new();
    trades.ignored = harness
        .iter()
        .filter(|record| record.kind == "ignored" && record.of(name))
        .count();
    for click in harness
        .iter()
        .filter(|record| record.kind == "click" && record.of(name))
    {
        trades.received += 1;
        let (window, state, raw) = (
            click.number("window"),
            click.number("state"),
            click.number("raw"),
        );
        let found = asks.iter().enumerate().skip(next).find(|(_, ask)| {
            ask.sent && Some(i64::from(ask.window)) == window && Some(i64::from(ask.slot)) == raw
        });
        let Some((at, ask)) = found else {
            trades
                .other_screens
                .push(format!("never asked for: {click:?}"));
            continue;
        };
        next = at + 1;
        // The window matched, or the server wouldn't have handled it; the
        // state the packet carried must be the one the script saw.
        let sent = click.number("sent");
        if Some(i64::from(ask.state)) != sent {
            trades.other_screens.push(format!(
                "the script saw window {} in state {}, its packet carried {sent:?}: {click:?}",
                ask.window, ask.state
            ));
        } else if sent != state {
            trades.changed_in_flight.push(format!(
                "sent onto state {}, met state {state:?}: {click:?}",
                ask.state
            ));
        }
        let on_a_listing = click.get("screen") == Some(HOUSE) && raw.is_some_and(|raw| raw < 45);
        let Some(seen) = ask.price.filter(|_| on_a_listing) else {
            continue;
        };
        trades.picks += 1;
        let met = click.get("listing").and_then(|id| prices.get(id)).copied();
        if met.is_some_and(|met| !same_price(met, seen)) {
            trades.other_listings.push(format!(
                "the script saw {seen}, the click met {met:?}: {click:?}"
            ));
        }
        picks.push((seen, false));
    }
    picks
}

/// Each purchase of the bot's takes the earliest pick of its price not yet
/// taken; one that finds none is a double buy.
fn take_the_purchases(
    harness: &[Record],
    name: &str,
    picks: &mut [(f64, bool)],
    trades: &mut Trades,
) {
    for purchase in harness
        .iter()
        .filter(|record| record.kind == "bought" && record.of(name))
    {
        trades.bought += 1;
        let price: Option<f64> = purchase.get("price").and_then(|price| price.parse().ok());
        let taken = price.and_then(|price| {
            picks
                .iter_mut()
                .find(|(seen, taken)| !*taken && same_price(*seen, price))
        });
        match taken {
            Some((_, taken)) => *taken = true,
            None => trades
                .double_buys
                .push(format!("no pick of the script's meant {purchase:?}")),
        }
    }
}

/// Each pick that bought nothing takes a miss line naming its price; one
/// that finds none is a silent miss. A miss line for a listing only bought
/// is the script's word wrong the other way.
fn name_the_misses(picks: &[(f64, bool)], trades: &mut Trades) {
    let mut said: Vec<(f64, bool)> = trades
        .misses
        .iter()
        .filter_map(|line| Some((missed_price(line)?, false)))
        .collect();
    for (seen, _) in picks.iter().filter(|(_, taken)| !*taken) {
        match said
            .iter_mut()
            .find(|(price, used)| !*used && same_price(*price, *seen))
        {
            Some((_, used)) => *used = true,
            None => trades.silent_misses.push(format!(
                "a pick of {seen} bought nothing, and no line names it"
            )),
        }
    }
    trades.missed_but_bought = trades
        .misses
        .iter()
        .filter(|line| {
            missed_price(line).is_some_and(|price| {
                let bought = picks
                    .iter()
                    .any(|(seen, taken)| *taken && same_price(*seen, price));
                let missed = picks
                    .iter()
                    .any(|(seen, taken)| !*taken && same_price(*seen, price));
                bought && !missed
            })
        })
        .cloned()
        .collect();
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const ON_THE_HOUSE: &str = r#"Act(Click(Click { seen: Seen { screen: ScreenId { window: WindowId(3), opening: 5 }, state: StateId(7), cursor: None }, kind: Left, slot: 13, touched: [(13, Some(Item { id: Identifier("minecraft:diamond"), count: 1, name: ServerText("Diamond"), lore: Lore([ServerText("Price: $1,501.37"), ServerText("Seller: Seller2")]) }))] }))"#;
    const NEXT_PAGE: &str = r#"Act(Click(Click { seen: Seen { screen: ScreenId { window: WindowId(3), opening: 5 }, state: StateId(8), cursor: None }, kind: Left, slot: 53, touched: [(53, Some(Item { id: Identifier("minecraft:arrow"), count: 1, name: ServerText("Next page"), lore: Lore([]) }))] }))"#;

    fn command(text: &str) -> Value {
        json!({ "message": SCREEN_COMMAND, "command": text })
    }

    fn logged(entry: &str) -> Value {
        json!({ "message": LOGGED, "entry": entry })
    }

    fn record(line: &str) -> Record {
        Record::parse(line).unwrap()
    }

    /// A click the harness received: its packet carried `window` and `sent`,
    /// and met the server's `state`.
    fn met(window: u32, sent: u32, state: u32, raw: u32, listing: &str) -> Record {
        record(&format!(
            "harness-click player=AfkBot1 screen=\"Auction House\" raw={raw} slot={raw} click=LEFT action=PICKUP_ALL hotbar=-1 cancelled=true window={window} state={state} sent={sent} listing={listing} tick=1 nanos=1"
        ))
    }

    /// A click that met the state its packet carried.
    fn click(window: u32, state: u32, raw: u32, listing: &str) -> Record {
        met(window, state, state, raw, listing)
    }

    #[test]
    fn a_click_command_reads_as_its_window_state_slot_and_seen_price() {
        assert_eq!(
            asked(ON_THE_HOUSE),
            Some(Asked {
                window: 3,
                state: 7,
                slot: 13,
                price: Some(1501.37),
                sent: true,
            })
        );
        assert_eq!(
            asked("Close(ScreenId { window: WindowId(3), opening: 5 })"),
            None
        );
    }

    #[test]
    fn a_pick_bought_and_a_page_turned_count_clean() {
        let harness = [
            record("harness-listed id=9 seller=Seller2 price=1501.37"),
            click(3, 7, 13, "9"),
            record("harness-bought id=9 buyer=AfkBot1 seller=Seller2 price=1501.37"),
            click(3, 8, 53, "-"),
        ];
        let agent = [
            command(ON_THE_HOUSE),
            logged("Menu { asked: Click, slot: Some(13), reason: Rejected, sent: true }"),
            command(NEXT_PAGE),
        ];

        let trades = count(&harness, &agent, "AfkBot1");

        assert_eq!((trades.asked, trades.sent, trades.received), (2, 2, 2));
        assert_eq!((trades.picks, trades.bought), (1, 1));
        assert_eq!(trades.unheard, 0);
        assert_eq!(trades.fates.get("ok"), Some(&1));
        assert_eq!(trades.fates.get("rejected"), Some(&1));
        assert_eq!(trades.double_buys, Vec::<String>::new());
        assert_eq!(trades.silent_misses, Vec::<String>::new());
        assert_eq!(trades.other_screens, Vec::<String>::new());
    }

    #[test]
    fn a_click_whose_session_ended_first_got_no_fate_and_is_no_ok() {
        let agent = [
            command(ON_THE_HOUSE),
            logged("Menu { asked: Click, slot: Some(13), reason: Rejected, sent: true }"),
            command(NEXT_PAGE),
            logged(r#"Line("Answered 1 clicks.")"#),
        ];

        let trades = count(&[], &agent, "AfkBot1");

        assert_eq!(trades.sent, 2);
        assert_eq!(trades.fates.get("ok"), Some(&0));
        assert_eq!(trades.unheard, 1);
    }

    #[test]
    fn a_pick_that_bought_nothing_needs_a_line_naming_its_price() {
        let harness = [click(3, 7, 13, "9")];
        let with_a_line = [
            command(ON_THE_HOUSE),
            logged(r#"Line("Missed 1501.37 from Seller: Seller2: no confirm screen came.")"#),
        ];
        let without = [command(ON_THE_HOUSE)];

        let logged = count(&harness, &with_a_line, "AfkBot1");
        let silent = count(&harness, &without, "AfkBot1");

        assert_eq!(logged.silent_misses, Vec::<String>::new());
        assert_eq!(logged.misses.len(), 1);
        assert_eq!(silent.silent_misses.len(), 1);
    }

    #[test]
    fn a_purchase_no_pick_meant_is_a_double_buy() {
        let harness = [
            click(3, 7, 13, "9"),
            record("harness-bought id=9 buyer=AfkBot1 seller=Seller2 price=1501.37"),
            record("harness-bought id=10 buyer=AfkBot1 seller=Seller3 price=1502.00"),
            record("harness-bought id=11 buyer=AfkBot2 seller=Seller3 price=1503.00"),
        ];

        let trades = count(&harness, &[command(ON_THE_HOUSE)], "AfkBot1");

        assert_eq!(trades.bought, 2);
        assert_eq!(trades.double_buys.len(), 1, "{:?}", trades.double_buys);
    }

    #[test]
    fn a_packet_in_another_state_or_a_click_never_asked_for_is_another_screen() {
        let harness = [
            click(3, 9, 13, "9"),
            click(4, 7, 53, "-"),
            click(3, 8, 53, "-"),
        ];
        let agent = [command(ON_THE_HOUSE), command(NEXT_PAGE)];

        let trades = count(&harness, &agent, "AfkBot1");

        // The first's packet carried state 9 where the script saw 7; the
        // second was never asked for in window 4; the third matches.
        assert_eq!(trades.other_screens.len(), 2, "{:#?}", trades.other_screens);
    }

    #[test]
    fn a_click_sent_onto_the_state_seen_that_met_a_newer_one_changed_in_flight() {
        let harness = [
            met(3, 7, 9, 13, "9"),
            record("harness-ignored player=AfkBot1 window=2 sent=4 raw=53 tick=1 nanos=1"),
        ];

        let trades = count(&harness, &[command(ON_THE_HOUSE)], "AfkBot1");

        assert_eq!(trades.other_screens, Vec::<String>::new());
        assert_eq!(trades.changed_in_flight.len(), 1);
        assert_eq!(trades.ignored, 1);
    }

    #[test]
    fn a_click_the_session_refused_matches_nothing_the_harness_received() {
        let harness = [click(3, 8, 53, "-")];
        let agent = [
            command(NEXT_PAGE),
            logged("Menu { asked: Click, slot: Some(53), reason: ContentChanged, sent: false }"),
            command(NEXT_PAGE),
        ];

        let trades = count(&harness, &agent, "AfkBot1");

        assert_eq!((trades.asked, trades.sent), (2, 1));
        assert_eq!(trades.not_sent.get("content_changed"), Some(&1));
        assert_eq!(trades.other_screens, Vec::<String>::new());
    }

    #[test]
    fn a_miss_line_for_a_listing_bought_is_the_scripts_word_wrong() {
        let harness = [
            click(3, 7, 13, "9"),
            record("harness-bought id=9 buyer=AfkBot1 seller=Seller2 price=1501.37"),
        ];
        let agent = [
            command(ON_THE_HOUSE),
            logged(r#"Line("Missed 1501.37 from Seller: Seller2: it didn't go through.")"#),
        ];

        let trades = count(&harness, &agent, "AfkBot1");

        assert_eq!(trades.missed_but_bought.len(), 1);
        assert_eq!(trades.silent_misses, Vec::<String>::new());
    }
}
