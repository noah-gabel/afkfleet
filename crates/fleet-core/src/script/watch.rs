//! [`ChatWatch`]: which of a scripted mode's chat patterns count right now
//! (ADR-0037, C4, C5).

use core::time::Duration;

use chrono::{DateTime, Utc};

use super::Ident;
use super::pattern::{ChatMatch, ChatPattern, ChatPatterns, When};
use crate::time;

/// Names one command a script asked to send, so the windows that wait for
/// it open when that command, and no other, is handed to the server. The
/// runtime numbers its sends; the number means nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SendId(pub u64);

/// Why a window can't wait for a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WindowError {
    /// The window isn't [`ChatWatch::MIN_WINDOW`] to
    /// [`ChatWatch::MAX_WINDOW`] long.
    #[error("a reply's window must be 250 ms to 30 s long")]
    Length,
}

/// What [`ChatWatch::observe`] made of a line.
#[derive(Debug, Clone, PartialEq)]
pub enum Observed {
    /// No pattern that counts now fits the line.
    Nothing,
    /// A pattern fits, and it counts: its window was open and is closed
    /// now, or it may come at any time.
    Match(ChatMatch),
    /// An any-time pattern fits, but its last match was less than
    /// [`ChatWatch::ANY_TIME_GAP`] ago: dropped, for the runtime to count.
    TooSoon(Ident),
}

/// Which chat patterns count right now, for one bot's script (ADR-0037).
///
/// - **A reply pattern counts only inside its window.** A script asks for
///   one right after a command of its own ([`expect`](Self::expect)); the
///   window opens when that command is handed to the server
///   ([`sent`](Self::sent)), never before and never if it isn't
///   ([`dropped`](Self::dropped)), and the first match closes it. So a
///   reply is read only for seconds after a command the script itself
///   sent.
/// - **An any-time pattern counts whenever the bot is online,** at most
///   once every [`ChatWatch::ANY_TIME_GAP`].
///
/// A pattern holds one window at a time: asking again replaces the one
/// that waits or is open. The watch holds no clock; every call takes `now`
/// (ADR-0010). A clock that goes backwards opens and closes nothing early.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatWatch {
    /// Windows that wait for their command to be sent.
    waiting: Vec<Waiting>,
    /// Open windows.
    open: Vec<Open>,
    /// When each any-time pattern last matched.
    last_any_time: Vec<(Ident, DateTime<Utc>)>,
}

/// A window that waits for its command.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Waiting {
    send: SendId,
    key: Ident,
    length: Duration,
}

/// An open window.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Open {
    key: Ident,
    until: DateTime<Utc>,
}

impl ChatWatch {
    /// The shortest window.
    pub const MIN_WINDOW: Duration = Duration::from_millis(250);
    /// The longest window.
    pub const MAX_WINDOW: Duration = Duration::from_secs(30);
    /// The shortest time between two matches of one any-time pattern.
    pub const ANY_TIME_GAP: Duration = Duration::from_secs(1);

    /// A watch with no window, as at a bot's start.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks for a window of `length` for the reply pattern `key`, to open
    /// when the command `send` is sent. It replaces a window of that
    /// pattern that waits or is open.
    ///
    /// The caller has checked that `key` is a declared reply pattern's and
    /// that `send` is a declared command of this hook; the watch only keeps
    /// the time.
    ///
    /// # Errors
    /// [`WindowError::Length`] for a length outside
    /// [`ChatWatch::MIN_WINDOW`] to [`ChatWatch::MAX_WINDOW`]; nothing
    /// changes then.
    pub fn expect(
        &mut self,
        send: SendId,
        key: Ident,
        length: Duration,
    ) -> Result<(), WindowError> {
        if !(Self::MIN_WINDOW..=Self::MAX_WINDOW).contains(&length) {
            return Err(WindowError::Length);
        }
        self.waiting.retain(|waiting| waiting.key != key);
        self.open.retain(|open| open.key != key);
        self.waiting.push(Waiting { send, key, length });
        Ok(())
    }

    /// The command `send` was handed to the server at `now`: the windows
    /// that waited for it open, each for its length. Returns their keys,
    /// for the script's log.
    pub fn sent(&mut self, send: SendId, now: DateTime<Utc>) -> Vec<Ident> {
        let (opening, waiting): (Vec<_>, Vec<_>) = core::mem::take(&mut self.waiting)
            .into_iter()
            .partition(|waiting| waiting.send == send);
        self.waiting = waiting;
        opening
            .into_iter()
            .map(|Waiting { key, length, .. }| {
                self.open.push(Open {
                    key: key.clone(),
                    until: time::add(now, length),
                });
                key
            })
            .collect()
    }

    /// The command `send` wasn't sent: the windows that waited for it are
    /// gone. Returns their keys, for the script's log.
    pub fn dropped(&mut self, send: SendId) -> Vec<Ident> {
        let (dropped, waiting): (Vec<_>, Vec<_>) = core::mem::take(&mut self.waiting)
            .into_iter()
            .partition(|waiting| waiting.send == send);
        self.waiting = waiting;
        dropped.into_iter().map(|waiting| waiting.key).collect()
    }

    /// Removes the windows that have run out at `now` and returns their
    /// keys, for the script's log.
    pub fn expired(&mut self, now: DateTime<Utc>) -> Vec<Ident> {
        let (open, expired): (Vec<_>, Vec<_>) = core::mem::take(&mut self.open)
            .into_iter()
            .partition(|open| open_at(open.until, now));
        self.open = open;
        expired.into_iter().map(|open| open.key).collect()
    }

    /// When the next open window runs out, for the runtime's timer.
    #[must_use]
    pub fn next_expiry(&self) -> Option<DateTime<Utc>> {
        self.open.iter().map(|open| open.until).min()
    }

    /// Whether `pattern` counts at `now`: a reply pattern while its window
    /// is open, an any-time pattern always. (Its gap is
    /// [`observe`](Self::observe)'s to judge.)
    #[must_use]
    pub fn counts(&self, pattern: &ChatPattern, now: DateTime<Utc>) -> bool {
        match pattern.when() {
            When::AnyTime => true,
            When::Reply => self
                .open
                .iter()
                .any(|open| &open.key == pattern.key() && open_at(open.until, now)),
        }
    }

    /// Puts `line`, a candidate line, to `patterns` at `now`: the first
    /// pattern in declared order that counts and fits. A reply's match
    /// closes its window; an any-time pattern's match within
    /// [`ChatWatch::ANY_TIME_GAP`] of its last is [`Observed::TooSoon`].
    pub fn observe(&mut self, patterns: &ChatPatterns, line: &str, now: DateTime<Utc>) -> Observed {
        let Some(found) = patterns.first_match(line, |pattern| self.counts(pattern, now)) else {
            return Observed::Nothing;
        };
        let any_time = patterns
            .get(found.key.as_str())
            .is_some_and(|pattern| pattern.when() == When::AnyTime);
        if !any_time {
            self.open.retain(|open| open.key != found.key);
            return Observed::Match(found);
        }
        let last = self
            .last_any_time
            .iter_mut()
            .find(|(key, _)| *key == found.key);
        match last {
            // `elapsed` is zero for a clock that went backwards: too soon.
            Some((_, last)) if time::elapsed(*last, now) < Self::ANY_TIME_GAP => {
                Observed::TooSoon(found.key)
            }
            Some((_, last)) => {
                *last = now;
                Observed::Match(found)
            }
            None => {
                self.last_any_time.push((found.key.clone(), now));
                Observed::Match(found)
            }
        }
    }

    /// Forgets every window, waiting or open: at a disconnect and when the
    /// script stops. When each any-time pattern last matched is kept, so a
    /// reconnect never shortens the gap.
    pub fn clear(&mut self) {
        self.waiting.clear();
        self.open.clear();
    }

    /// Whether no window waits or is open.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.waiting.is_empty() && self.open.is_empty()
    }
}

/// Whether a window that runs `until` is still open at `now`.
fn open_at(until: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    // `elapsed` is zero while `now` is at or before `until`.
    time::elapsed(until, now) == Duration::ZERO
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::{CaptureDraft, CaptureKind, Captured, PatternDraft};
    use proptest::prelude::*;
    use rstest::rstest;

    const START_SECS: i64 = 1_700_000_000;

    /// `ms` milliseconds after the start.
    fn at(ms: u64) -> DateTime<Utc> {
        time::add(
            DateTime::from_timestamp(START_SECS, 0).unwrap(),
            Duration::from_millis(ms),
        )
    }

    const fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn key(name: &str) -> Ident {
        Ident::try_from(name).unwrap()
    }

    fn pattern(name: &str, when: When, text: &str) -> ChatPattern {
        ChatPattern::try_from(PatternDraft {
            key: name.to_owned(),
            when,
            text: text.to_owned(),
            captures: vec![CaptureDraft {
                name: "n".to_owned(),
                kind: CaptureKind::Integer { min: 0, max: 999 },
            }],
        })
        .unwrap()
    }

    /// A reply pattern `balance` and an any-time pattern `tip`.
    fn patterns() -> ChatPatterns {
        ChatPatterns::try_new(vec![
            pattern("balance", When::Reply, "Balance: {n} coins"),
            pattern("tip", When::AnyTime, "You got {n} coins"),
        ])
        .unwrap()
    }

    const REPLY: &str = "Balance: 500 coins";
    const TIP: &str = "You got 5 coins";

    fn matched(name: &str, n: i64) -> Observed {
        Observed::Match(ChatMatch {
            key: key(name),
            values: vec![(key("n"), Captured::Integer(n))],
        })
    }

    /// A watch whose `balance` window opened at 1,000 ms for 5 s.
    fn open_watch() -> ChatWatch {
        let mut watch = ChatWatch::new();
        watch.expect(SendId(1), key("balance"), ms(5_000)).unwrap();
        assert_eq!(watch.sent(SendId(1), at(1_000)), [key("balance")]);
        watch
    }

    #[test]
    fn a_reply_without_a_window_matches_nothing() {
        let mut watch = ChatWatch::new();

        assert_eq!(watch.observe(&patterns(), REPLY, at(0)), Observed::Nothing);
        assert!(watch.is_idle());
    }

    #[test]
    fn a_reply_inside_its_window_matches_and_closes_it() {
        let mut watch = open_watch();

        assert_eq!(
            watch.observe(&patterns(), REPLY, at(2_000)),
            matched("balance", 500)
        );
        // One reply an `expect`: the same line a moment later is nothing.
        assert_eq!(
            watch.observe(&patterns(), REPLY, at(2_001)),
            Observed::Nothing
        );
        assert!(watch.is_idle());
    }

    #[rstest]
    #[case::at_its_opening(1_000, true)]
    #[case::inside(3_000, true)]
    #[case::at_its_last_millisecond(6_000, true)]
    #[case::a_millisecond_late(6_001, false)]
    #[case::long_after(60_000, false)]
    fn a_window_is_open_for_its_length_and_no_longer(#[case] now: u64, #[case] open: bool) {
        let mut watch = open_watch();
        let balance = patterns().get("balance").cloned().unwrap();

        assert_eq!(watch.counts(&balance, at(now)), open);
        let expected = if open {
            matched("balance", 500)
        } else {
            Observed::Nothing
        };
        assert_eq!(watch.observe(&patterns(), REPLY, at(now)), expected);
    }

    #[test]
    fn a_window_opens_only_when_its_command_is_sent() {
        let mut watch = ChatWatch::new();
        watch.expect(SendId(7), key("balance"), ms(5_000)).unwrap();

        // Asked for, not sent yet: the reply counts for nothing.
        assert!(!watch.is_idle());
        assert_eq!(
            watch.observe(&patterns(), REPLY, at(100)),
            Observed::Nothing
        );
        // Another command's send opens nothing.
        assert_eq!(watch.sent(SendId(8), at(200)), []);
        assert_eq!(
            watch.observe(&patterns(), REPLY, at(300)),
            Observed::Nothing
        );

        // Its own, sent 20 s later: the window runs from then.
        assert_eq!(watch.sent(SendId(7), at(20_000)), [key("balance")]);
        assert_eq!(watch.next_expiry(), Some(at(25_000)));
        assert_eq!(
            watch.observe(&patterns(), REPLY, at(24_000)),
            matched("balance", 500)
        );
    }

    #[test]
    fn a_command_that_isnt_sent_opens_no_window() {
        let mut watch = ChatWatch::new();
        watch.expect(SendId(7), key("balance"), ms(5_000)).unwrap();

        assert_eq!(watch.dropped(SendId(8)), []);
        assert_eq!(watch.dropped(SendId(7)), [key("balance")]);

        assert!(watch.is_idle());
        assert_eq!(watch.sent(SendId(7), at(100)), []);
        assert_eq!(
            watch.observe(&patterns(), REPLY, at(200)),
            Observed::Nothing
        );
    }

    #[test]
    fn a_window_that_runs_out_is_reported_once() {
        let mut watch = open_watch();

        assert_eq!(watch.expired(at(6_000)), []);
        assert_eq!(watch.next_expiry(), Some(at(6_000)));
        assert_eq!(watch.expired(at(6_001)), [key("balance")]);
        assert_eq!(watch.expired(at(7_000)), []);
        assert_eq!(watch.next_expiry(), None);
        assert!(watch.is_idle());
    }

    #[test]
    fn a_matched_window_never_runs_out() {
        let mut watch = open_watch();
        assert_eq!(
            watch.observe(&patterns(), REPLY, at(2_000)),
            matched("balance", 500)
        );

        assert_eq!(watch.expired(at(60_000)), []);
    }

    #[rstest]
    #[case::too_short(249)]
    #[case::none(0)]
    #[case::too_long(30_001)]
    #[case::far_too_long(86_400_000)]
    fn a_window_outside_250_ms_to_30_s_is_refused(#[case] length: u64) {
        let mut watch = ChatWatch::new();

        assert_eq!(
            watch.expect(SendId(1), key("balance"), ms(length)),
            Err(WindowError::Length)
        );
        assert!(watch.is_idle());
        assert_eq!(watch.sent(SendId(1), at(0)), []);
    }

    #[rstest]
    #[case::the_shortest(250)]
    #[case::the_longest(30_000)]
    fn a_window_of_250_ms_to_30_s_is_taken(#[case] length: u64) {
        let mut watch = ChatWatch::new();

        assert_eq!(watch.expect(SendId(1), key("balance"), ms(length)), Ok(()));
        assert_eq!(watch.sent(SendId(1), at(0)), [key("balance")]);
        assert_eq!(watch.next_expiry(), Some(at(length)));
    }

    #[test]
    fn asking_again_replaces_the_patterns_window() {
        // A waiting one: only the later command opens the window.
        let mut watch = ChatWatch::new();
        watch.expect(SendId(1), key("balance"), ms(5_000)).unwrap();
        watch.expect(SendId(2), key("balance"), ms(1_000)).unwrap();
        assert_eq!(watch.sent(SendId(1), at(0)), []);
        assert_eq!(watch.sent(SendId(2), at(100)), [key("balance")]);
        assert_eq!(watch.next_expiry(), Some(at(1_100)));

        // An open one: the new window's time replaces the old one's.
        let mut watch = open_watch();
        watch.expect(SendId(2), key("balance"), ms(1_000)).unwrap();
        assert_eq!(watch.sent(SendId(2), at(2_000)), [key("balance")]);
        assert_eq!(watch.next_expiry(), Some(at(3_000)));
        assert_eq!(
            watch.observe(&patterns(), REPLY, at(3_001)),
            Observed::Nothing
        );
    }

    #[test]
    fn one_command_opens_every_window_that_waits_for_it() {
        let mut watch = ChatWatch::new();
        watch.expect(SendId(1), key("balance"), ms(5_000)).unwrap();
        watch.expect(SendId(1), key("other"), ms(1_000)).unwrap();

        assert_eq!(watch.sent(SendId(1), at(0)), [key("balance"), key("other")]);
        assert_eq!(watch.next_expiry(), Some(at(1_000)));
        assert_eq!(watch.expired(at(1_001)), [key("other")]);
        assert_eq!(watch.next_expiry(), Some(at(5_000)));
    }

    #[test]
    fn an_any_time_pattern_counts_without_a_window() {
        let mut watch = ChatWatch::new();
        let tip = patterns().get("tip").cloned().unwrap();

        assert!(watch.counts(&tip, at(0)));
        assert_eq!(watch.observe(&patterns(), TIP, at(0)), matched("tip", 5));
        assert!(watch.is_idle());
    }

    #[rstest]
    #[case::at_once(0, false)]
    #[case::inside_the_gap(999, false)]
    #[case::after_the_gap(1_000, true)]
    #[case::long_after(60_000, true)]
    fn an_any_time_pattern_matches_once_a_second(#[case] later: u64, #[case] again: bool) {
        let mut watch = ChatWatch::new();
        assert_eq!(
            watch.observe(&patterns(), TIP, at(5_000)),
            matched("tip", 5)
        );

        let expected = if again {
            matched("tip", 5)
        } else {
            Observed::TooSoon(key("tip"))
        };
        assert_eq!(watch.observe(&patterns(), TIP, at(5_000 + later)), expected);
    }

    /// A dropped match doesn't count as one: the gap runs from the last
    /// match, so a steady flood still lets one line a second through.
    #[test]
    fn a_dropped_match_doesnt_push_the_gap_on() {
        let mut watch = ChatWatch::new();
        assert_eq!(watch.observe(&patterns(), TIP, at(0)), matched("tip", 5));
        for now in [200, 400, 600, 800] {
            assert_eq!(
                watch.observe(&patterns(), TIP, at(now)),
                Observed::TooSoon(key("tip"))
            );
        }

        assert_eq!(
            watch.observe(&patterns(), TIP, at(1_000)),
            matched("tip", 5)
        );
    }

    #[test]
    fn a_line_that_fits_nothing_changes_nothing() {
        let mut watch = open_watch();
        let before = watch.clone();

        assert_eq!(
            watch.observe(&patterns(), "Balance: lots", at(2_000)),
            Observed::Nothing
        );
        assert_eq!(
            watch.observe(&patterns(), "<AfkBot2> Balance: 500 coins", at(2_000)),
            Observed::Nothing
        );

        assert_eq!(watch, before);
    }

    #[test]
    fn clearing_forgets_every_window_and_keeps_the_any_time_gap() {
        let mut watch = open_watch();
        watch.expect(SendId(2), key("other"), ms(1_000)).unwrap();
        assert_eq!(
            watch.observe(&patterns(), TIP, at(1_500)),
            matched("tip", 5)
        );

        watch.clear();

        assert!(watch.is_idle());
        assert_eq!(watch.next_expiry(), None);
        assert_eq!(
            watch.observe(&patterns(), REPLY, at(1_600)),
            Observed::Nothing
        );
        assert_eq!(watch.sent(SendId(2), at(1_700)), []);
        assert_eq!(
            watch.observe(&patterns(), TIP, at(1_800)),
            Observed::TooSoon(key("tip"))
        );
    }

    #[test]
    fn a_clock_that_goes_backwards_opens_nothing_early() {
        let mut watch = open_watch();

        // Before the window opened, by the clock: still open, never closed
        // early, and nothing has run out.
        assert_eq!(watch.expired(at(0)), []);
        assert_eq!(
            watch.observe(&patterns(), REPLY, at(0)),
            matched("balance", 500)
        );

        // An any-time match "before" the last one is inside the gap.
        let mut watch = ChatWatch::new();
        assert_eq!(
            watch.observe(&patterns(), TIP, at(5_000)),
            matched("tip", 5)
        );
        assert_eq!(
            watch.observe(&patterns(), TIP, at(0)),
            Observed::TooSoon(key("tip"))
        );
    }

    #[test]
    fn the_error_names_the_range() {
        assert_eq!(
            WindowError::Length.to_string(),
            "a reply's window must be 250 ms to 30 s long"
        );
        assert_eq!(ChatWatch::MIN_WINDOW, ms(250));
        assert_eq!(ChatWatch::MAX_WINDOW, ms(30_000));
        assert_eq!(ChatWatch::ANY_TIME_GAP, ms(1_000));
    }

    /// One step of a script's life, for the property below.
    #[derive(Debug, Clone)]
    enum Step {
        Expect { send: u64, length: u64 },
        Sent { send: u64 },
        Dropped { send: u64 },
        Line,
        Expire,
        Clear,
    }

    fn any_step() -> impl Strategy<Value = Step> {
        prop_oneof![
            (0_u64..4, 0_u64..40_000).prop_map(|(send, length)| Step::Expect { send, length }),
            (0_u64..4).prop_map(|send| Step::Sent { send }),
            (0_u64..4).prop_map(|send| Step::Dropped { send }),
            Just(Step::Line),
            Just(Step::Expire),
            Just(Step::Clear),
        ]
    }

    proptest! {
        /// Whatever a script and the runtime do, in any order and at any
        /// times: a reply matches only within 30 s after one of its
        /// commands was sent, and at most once for each send.
        #[test]
        fn a_reply_matches_only_inside_a_window_a_sent_command_opened(
            steps in proptest::collection::vec((any_step(), 0_u64..10_000), 0..40),
        ) {
            let patterns = patterns();
            let mut watch = ChatWatch::new();
            let mut now = 0_u64;
            // When a command with a window waiting was last sent, and
            // whether a reply was matched since.
            let mut last_sent: Option<u64> = None;
            let mut waiting: Vec<u64> = Vec::new();
            for (step, pause) in steps {
                now += pause;
                match step {
                    Step::Expect { send, length } => {
                        if watch.expect(SendId(send), key("balance"), ms(length)).is_ok() {
                            prop_assert!((250..=30_000).contains(&length));
                            waiting = vec![send];
                            last_sent = None;
                        }
                    }
                    Step::Sent { send } => {
                        let opened = watch.sent(SendId(send), at(now));
                        prop_assert_eq!(!opened.is_empty(), waiting.contains(&send));
                        if !opened.is_empty() {
                            waiting.clear();
                            last_sent = Some(now);
                        }
                    }
                    Step::Dropped { send } => {
                        let gone = watch.dropped(SendId(send));
                        prop_assert_eq!(!gone.is_empty(), waiting.contains(&send));
                        waiting.retain(|waits| *waits != send);
                    }
                    Step::Line => {
                        if let Observed::Match(found) = watch.observe(&patterns, REPLY, at(now)) {
                            prop_assert_eq!(found.key.as_str(), "balance");
                            let sent = last_sent.take();
                            prop_assert!(sent.is_some_and(|sent| now - sent <= 30_000), "{sent:?} {now}");
                        }
                    }
                    Step::Expire => {
                        if !watch.expired(at(now)).is_empty() {
                            last_sent = None;
                        }
                    }
                    Step::Clear => {
                        watch.clear();
                        waiting.clear();
                        last_sent = None;
                    }
                }
            }
        }
    }
}
