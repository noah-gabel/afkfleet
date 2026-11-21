//! [`ScriptGate`]: how much the server takes in of what an agent reports
//! about a bot's script (Plan.md P15.23; ADR-0036, D13).
//!
//! An agent is trusted in nothing (ADR-0019): whatever one sends, a bot's
//! script reaches the live events at a bounded rate. Each bot has two
//! buckets, one for its script's status and one for its log, each a burst
//! that refills at a steady rate (the generic cell rate: a "theoretical
//! arrival time" a bucket, in milliseconds of the server's own clock, the
//! time the report was received). What's over is dropped and counted; the
//! status is said again at the bot's next join, so a dropped one heals.
//!
//! - **The bounds** sit well above what an honest agent sends: a script
//!   logs 20 lines a minute itself, the runtime 20 refusals and what the
//!   bot's chat rate and its patterns' one match a second allow; its status
//!   comes at a start, a stop and every join.
//! - **The gate is shared by every stream,** so an agent that reconnects
//!   resets nothing.
//! - **It stays small by itself:** a bot's buckets are kept only while one
//!   isn't full again, and a bucket that's full is exactly a new one, so
//!   forgetting it changes nothing. Past [`ScriptGate::SWEEP_AT`] bots, the
//!   full ones are forgotten.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use fleet_core::id::BotId;

/// What a report about a bot's script is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScriptReportKind {
    /// Where the script is in its life.
    Status,
    /// A line of its log.
    Log,
}

impl ScriptReportKind {
    /// Every kind, in a fixed order.
    pub const ALL: [Self; 2] = [Self::Status, Self::Log];

    /// Its name, for a metric's label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Log => "log",
        }
    }

    /// How many may come at once, and how many milliseconds give one more.
    const fn rate(self) -> (i64, i64) {
        match self {
            Self::Status => (ScriptGate::STATUS_BURST, ScriptGate::STATUS_EVERY_MS),
            Self::Log => (ScriptGate::LOG_BURST, ScriptGate::LOG_EVERY_MS),
        }
    }
}

/// One bot's two buckets: when each is full again, in milliseconds since
/// the Unix epoch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Buckets {
    status: i64,
    log: i64,
}

/// The gate; see the [module docs](self).
#[derive(Debug, Default)]
pub struct ScriptGate {
    bots: HashMap<BotId, Buckets>,
}

impl ScriptGate {
    /// A bot's statuses at once.
    pub const STATUS_BURST: i64 = 10;
    /// One more status every 10 s.
    pub const STATUS_EVERY_MS: i64 = 10_000;
    /// A bot's log lines at once.
    pub const LOG_BURST: i64 = 30;
    /// One more line a second: 60 a minute.
    pub const LOG_EVERY_MS: i64 = 1_000;
    /// How many bots the gate keeps before it forgets the full buckets.
    pub const SWEEP_AT: usize = 4096;

    /// An empty gate.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a report of `kind` about `bot`, received `at`, is taken in.
    pub fn admit(&mut self, bot: BotId, kind: ScriptReportKind, at: DateTime<Utc>) -> bool {
        let now = at.timestamp_millis();
        if self.bots.len() >= Self::SWEEP_AT {
            self.bots
                .retain(|_, buckets| buckets.status > now || buckets.log > now);
        }
        let buckets = self.bots.entry(bot).or_default();
        let full_at = match kind {
            ScriptReportKind::Status => &mut buckets.status,
            ScriptReportKind::Log => &mut buckets.log,
        };
        let (burst, every) = kind.rate();
        // An empty bucket is full again `burst` intervals from now; one more
        // report fits while it's full again within `burst - 1` of them.
        let from = (*full_at).max(now);
        if from.saturating_sub(now) > (burst.saturating_sub(1)).saturating_mul(every) {
            return false;
        }
        *full_at = from.saturating_add(every);
        true
    }

    /// How many bots it keeps now.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bots.len()
    }

    /// Whether it keeps none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bots.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;

    fn bot(n: u16) -> BotId {
        let mut random = [0; 10];
        random[..2].copy_from_slice(&n.to_be_bytes());
        BotId::new_v7(t0(), random).unwrap()
    }

    fn t0() -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000, 0).unwrap()
    }

    fn after(ms: i64) -> DateTime<Utc> {
        t0() + TimeDelta::milliseconds(ms)
    }

    /// How many of `count` reports at `at` pass.
    fn passed(
        gate: &mut ScriptGate,
        bot: BotId,
        kind: ScriptReportKind,
        at: DateTime<Utc>,
        count: u32,
    ) -> u32 {
        (0..count)
            .map(|_| u32::from(gate.admit(bot, kind, at)))
            .sum()
    }

    #[test]
    fn a_flood_passes_its_burst_and_then_its_rate() {
        let mut gate = ScriptGate::new();

        assert_eq!(
            passed(&mut gate, bot(1), ScriptReportKind::Log, t0(), 1_000),
            30
        );
        assert_eq!(
            passed(&mut gate, bot(1), ScriptReportKind::Log, after(999), 10),
            0
        );
        assert_eq!(
            passed(&mut gate, bot(1), ScriptReportKind::Log, after(1_000), 10),
            1
        );
        assert_eq!(
            passed(&mut gate, bot(1), ScriptReportKind::Status, t0(), 100),
            10
        );
        assert_eq!(
            passed(&mut gate, bot(1), ScriptReportKind::Status, after(9_999), 5),
            0
        );
        assert_eq!(
            passed(
                &mut gate,
                bot(1),
                ScriptReportKind::Status,
                after(10_000),
                5
            ),
            1
        );
    }

    #[test]
    fn over_a_minute_a_flooding_bot_gets_its_burst_and_one_a_second() {
        let mut gate = ScriptGate::new();

        let taken: u32 = (0..60_000)
            .step_by(10)
            .map(|ms| passed(&mut gate, bot(1), ScriptReportKind::Log, after(ms), 5))
            .sum();

        assert_eq!(taken, 30 + 59);
    }

    #[test]
    fn bots_and_kinds_have_their_own_buckets() {
        let mut gate = ScriptGate::new();
        passed(&mut gate, bot(1), ScriptReportKind::Log, t0(), 100);

        assert!(gate.admit(bot(1), ScriptReportKind::Status, t0()));
        assert!(gate.admit(bot(2), ScriptReportKind::Log, t0()));
    }

    #[test]
    fn a_clock_that_goes_back_frees_nothing() {
        let mut gate = ScriptGate::new();
        passed(&mut gate, bot(1), ScriptReportKind::Log, after(60_000), 30);

        assert_eq!(
            passed(&mut gate, bot(1), ScriptReportKind::Log, t0(), 10),
            0
        );
    }

    #[test]
    fn the_gate_forgets_only_full_buckets_which_changes_nothing() {
        let mut gate = ScriptGate::new();
        // One bot flooded, and many that sent one line long ago.
        passed(&mut gate, bot(0), ScriptReportKind::Log, after(100_000), 30);
        for n in 1..u16::try_from(ScriptGate::SWEEP_AT).unwrap() {
            assert!(gate.admit(bot(n), ScriptReportKind::Log, t0()));
        }
        assert_eq!(gate.len(), ScriptGate::SWEEP_AT);

        // A new bot's report sweeps: the long-ago ones are full again.
        assert!(gate.admit(bot(9_999), ScriptReportKind::Log, after(100_000)));

        assert_eq!(gate.len(), 2);
        assert!(!gate.admit(bot(0), ScriptReportKind::Log, after(100_000)));
        assert!(gate.admit(bot(1), ScriptReportKind::Log, after(100_000)));
    }

    #[test]
    fn every_kind_has_its_label() {
        let labels: Vec<&str> = ScriptReportKind::ALL
            .iter()
            .map(|kind| kind.label())
            .collect();

        assert_eq!(labels, ["status", "log"]);
        assert_eq!(ScriptGate::new().len(), 0);
    }
}
