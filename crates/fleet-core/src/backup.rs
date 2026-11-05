//! The backups' rules (Plan.md P12.3; ADR-0022, S3, S4, R1, R2): how often
//! the server takes one, how many it keeps, what a backup's file is called,
//! and which files go.
//!
//! - **The policy** ([`BackupPolicy`]): a backup every
//!   `backup_interval_hours` (1–168), the newest `backup_keep` (1–60) kept,
//!   and their product at most [`BackupPolicy::MAX_SPAN_HOURS`] (720 h, chat's
//!   30-day ceiling), which bounds how long a deleted line lives on in a
//!   backup on the host.
//! - **A name** ([`BackupName`]) is the second the backup was taken, in UTC:
//!   `afkfleet-backup-YYYYMMDDTHHMMSSZ.db` once finished, and the dot file
//!   `.afkfleet-backup-YYYYMMDDTHHMMSSZ.db.partial` while it's written, so a
//!   copier taking `afkfleet-backup-*.db` never takes a half-written one. A
//!   name is read back only when it re-formats exactly, so name order is time
//!   order and nothing else counts.
//! - **Due** ([`BackupPolicy::is_due`]): when no finished backup is younger
//!   than the interval. The age comes from the name, never the file's
//!   modification time, so a restart neither skips nor doubles one; a name
//!   dated more than [`FUTURE_SLACK`] ahead doesn't count.
//! - **Pruning** ([`BackupPolicy::to_prune`]): every finished backup but the
//!   newest `keep`, by name, and never the one just written or one newer
//!   than it, so neither a clock set back nor a second process backing up
//!   into the same folder loses a newer backup to it. A leftover temporary
//!   file, or SQLite's journal beside it, goes once it's [`STALE_PARTIAL`]
//!   old ([`is_stale_partial`]), so a backup still being written by another
//!   process is never touched.

use chrono::{DateTime, Datelike as _, NaiveDate, TimeDelta, Timelike as _, Utc};

/// How far ahead of the clock a finished backup's name may be dated and still
/// count: beyond it, the clock was set back after it was taken.
pub const FUTURE_SLACK: TimeDelta = TimeDelta::hours(1);

/// How old a leftover temporary file's name must be before pruning removes
/// it: no backup takes that long, so its writer is gone.
pub const STALE_PARTIAL: TimeDelta = TimeDelta::hours(24);

/// How often the server takes a backup and how many it keeps (S4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupPolicy {
    interval_hours: u32,
    keep: u32,
}

impl BackupPolicy {
    /// The shortest interval, in hours.
    pub const MIN_INTERVAL_HOURS: u32 = 1;
    /// The longest interval: a week.
    pub const MAX_INTERVAL_HOURS: u32 = 168;
    /// The interval when none is set: a day.
    pub const DEFAULT_INTERVAL_HOURS: u32 = 24;
    /// The fewest backups kept.
    pub const MIN_KEEP: u32 = 1;
    /// The most backups kept.
    pub const MAX_KEEP: u32 = 60;
    /// How many are kept when nothing is set: two weeks of daily ones.
    pub const DEFAULT_KEEP: u32 = 14;
    /// The longest span the kept backups may cover, `keep × interval`: 30
    /// days, chat's longest retention.
    pub const MAX_SPAN_HOURS: u32 = 720;

    /// Checks both values against their ranges, then their product.
    ///
    /// # Errors
    /// [`BackupPolicyError::Interval`] or [`BackupPolicyError::Keep`] for a
    /// value out of its range, then [`BackupPolicyError::Span`] when both are
    /// in range but cover more than [`BackupPolicy::MAX_SPAN_HOURS`].
    pub const fn try_new(interval_hours: u32, keep: u32) -> Result<Self, BackupPolicyError> {
        if interval_hours < Self::MIN_INTERVAL_HOURS || interval_hours > Self::MAX_INTERVAL_HOURS {
            return Err(BackupPolicyError::Interval);
        }
        if keep < Self::MIN_KEEP || keep > Self::MAX_KEEP {
            return Err(BackupPolicyError::Keep);
        }
        // At most 168 × 60 here, far below u32::MAX.
        if interval_hours * keep > Self::MAX_SPAN_HOURS {
            return Err(BackupPolicyError::Span);
        }
        Ok(Self {
            interval_hours,
            keep,
        })
    }

    /// The interval in hours.
    #[must_use]
    pub const fn interval_hours(&self) -> u32 {
        self.interval_hours
    }

    /// How many finished backups are kept.
    #[must_use]
    pub const fn keep(&self) -> u32 {
        self.keep
    }

    /// The interval.
    #[must_use]
    pub fn interval(&self) -> TimeDelta {
        TimeDelta::hours(i64::from(self.interval_hours))
    }

    /// Whether a backup is due at `now`, given the finished backups' names:
    /// when none dated up to [`FUTURE_SLACK`] ahead of `now` is younger than
    /// the interval.
    #[must_use]
    pub fn is_due(&self, finished: &[BackupName], now: DateTime<Utc>) -> bool {
        let latest = now.checked_add_signed(FUTURE_SLACK).unwrap_or(now);
        let newest = finished
            .iter()
            .map(BackupName::stamp)
            .filter(|stamp| *stamp <= latest)
            .max();
        match newest {
            None => true,
            Some(newest) => newest
                .checked_add_signed(self.interval())
                .is_some_and(|due| due <= now),
        }
    }

    /// The finished backups pruning deletes once `written` is done: all but
    /// the newest `keep` by name, newest first, and never `written` or a
    /// backup newer than it. A newer one, which another process wrote
    /// meanwhile or a clock set back left, counts toward the kept.
    #[must_use]
    pub fn to_prune(&self, finished: &[BackupName], written: BackupName) -> Vec<BackupName> {
        let mut others: Vec<BackupName> = finished
            .iter()
            .copied()
            .filter(|name| *name != written)
            .collect();
        others.sort_unstable_by(|a, b| b.cmp(a));
        others.dedup();
        // `written` is always one of the kept.
        let kept = usize::try_from(self.keep)
            .unwrap_or(usize::MAX)
            .saturating_sub(1);
        others
            .into_iter()
            .skip(kept)
            .filter(|name| *name < written)
            .collect()
    }
}

impl Default for BackupPolicy {
    fn default() -> Self {
        Self {
            interval_hours: Self::DEFAULT_INTERVAL_HOURS,
            keep: Self::DEFAULT_KEEP,
        }
    }
}

/// Why a [`BackupPolicy`] was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BackupPolicyError {
    /// The interval is outside 1–168 hours.
    #[error(
        "the interval must be {} to {} hours",
        BackupPolicy::MIN_INTERVAL_HOURS,
        BackupPolicy::MAX_INTERVAL_HOURS
    )]
    Interval,
    /// The count kept is outside 1–60.
    #[error(
        "must keep {} to {} backups",
        BackupPolicy::MIN_KEEP,
        BackupPolicy::MAX_KEEP
    )]
    Keep,
    /// The kept backups would cover more than 720 hours.
    #[error(
        "the kept backups times the interval must be at most {} hours",
        BackupPolicy::MAX_SPAN_HOURS
    )]
    Span,
}

/// A backup's name: the UTC second it was taken (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BackupName {
    stamp: DateTime<Utc>,
}

impl BackupName {
    /// What every finished backup's file name starts with.
    pub const PREFIX: &'static str = "afkfleet-backup-";
    /// What every finished backup's file name ends with.
    pub const SUFFIX: &'static str = ".db";
    /// What a temporary file's name adds after the finished name.
    pub const PARTIAL_SUFFIX: &'static str = ".partial";
    /// What SQLite adds to a file's name for its rollback journal.
    pub const JOURNAL_SUFFIX: &'static str = "-journal";

    /// The name of a backup taken at `now`, to the second; `None` for a time
    /// whose year doesn't have four digits.
    #[must_use]
    pub fn at(now: DateTime<Utc>) -> Option<Self> {
        let stamp = now.with_nanosecond(0)?;
        (0..=9999).contains(&stamp.year()).then_some(Self { stamp })
    }

    /// The second the backup was taken.
    #[must_use]
    pub const fn stamp(&self) -> DateTime<Utc> {
        self.stamp
    }

    /// The finished file's name, `afkfleet-backup-YYYYMMDDTHHMMSSZ.db`.
    #[must_use]
    pub fn file_name(&self) -> String {
        let stamp = self.stamp;
        format!(
            "{}{:04}{:02}{:02}T{:02}{:02}{:02}Z{}",
            Self::PREFIX,
            stamp.year(),
            stamp.month(),
            stamp.day(),
            stamp.hour(),
            stamp.minute(),
            stamp.second(),
            Self::SUFFIX
        )
    }

    /// The temporary file's name, `.afkfleet-backup-YYYYMMDDTHHMMSSZ.db.partial`.
    #[must_use]
    pub fn partial_name(&self) -> String {
        format!(".{}{}", self.file_name(), Self::PARTIAL_SUFFIX)
    }

    /// A finished backup's name read back: only one this module would write.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let stamp = name
            .strip_prefix(Self::PREFIX)?
            .strip_suffix(Self::SUFFIX)?;
        let number = |from: usize, to: usize| stamp.get(from..to)?.parse::<u32>().ok();
        if stamp.len() != 16 || stamp.get(8..9) != Some("T") || stamp.get(15..) != Some("Z") {
            return None;
        }
        let year = i32::try_from(number(0, 4)?).ok()?;
        let date = NaiveDate::from_ymd_opt(year, number(4, 6)?, number(6, 8)?)?;
        let time = date.and_hms_opt(number(9, 11)?, number(11, 13)?, number(13, 15)?)?;
        let parsed = Self {
            stamp: time.and_utc(),
        };
        // Only the exact form this module writes: no sign, no other digits.
        (parsed.file_name() == name).then_some(parsed)
    }

    /// A temporary file's name read back: only one this module would write.
    #[must_use]
    pub fn parse_partial(name: &str) -> Option<Self> {
        Self::parse(name.strip_prefix('.')?.strip_suffix(Self::PARTIAL_SUFFIX)?)
    }

    /// The rollback journal SQLite keeps beside the temporary file while it
    /// writes it, `.afkfleet-backup-YYYYMMDDTHHMMSSZ.db.partial-journal`.
    #[must_use]
    pub fn partial_journal_name(&self) -> String {
        format!("{}{}", self.partial_name(), Self::JOURNAL_SUFFIX)
    }

    /// A temporary file's journal's name read back: only one SQLite would
    /// write beside a temporary file of this module's.
    #[must_use]
    pub fn parse_partial_journal(name: &str) -> Option<Self> {
        Self::parse_partial(name.strip_suffix(Self::JOURNAL_SUFFIX)?)
    }
}

/// Whether a leftover temporary file named for `name` may go at `now`: once
/// it's [`STALE_PARTIAL`] old.
#[must_use]
pub fn is_stale_partial(name: BackupName, now: DateTime<Utc>) -> bool {
    name.stamp
        .checked_add_signed(STALE_PARTIAL)
        .is_some_and(|stale| stale <= now)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().to_utc()
    }

    fn name(text: &str) -> BackupName {
        BackupName::at(at(text)).unwrap()
    }

    #[rstest]
    #[case::the_defaults(24, 14)]
    #[case::the_smallest(1, 1)]
    #[case::a_week_four_times(168, 4)]
    #[case::hourly_for_a_month(1, 60)]
    #[case::exactly_the_span(12, 60)]
    fn a_policy_in_range_is_kept_as_given(#[case] interval: u32, #[case] keep: u32) {
        let policy = BackupPolicy::try_new(interval, keep).unwrap();

        assert_eq!((policy.interval_hours(), policy.keep()), (interval, keep));
        assert_eq!(policy.interval(), TimeDelta::hours(i64::from(interval)));
    }

    #[rstest]
    #[case::no_interval(0, 14, BackupPolicyError::Interval)]
    #[case::over_a_week(169, 1, BackupPolicyError::Interval)]
    #[case::none_kept(24, 0, BackupPolicyError::Keep)]
    #[case::too_many_kept(1, 61, BackupPolicyError::Keep)]
    #[case::over_the_span(24, 31, BackupPolicyError::Span)]
    #[case::a_week_five_times(168, 5, BackupPolicyError::Span)]
    fn a_policy_out_of_bounds_is_refused(
        #[case] interval: u32,
        #[case] keep: u32,
        #[case] error: BackupPolicyError,
    ) {
        assert_eq!(BackupPolicy::try_new(interval, keep), Err(error));
    }

    #[test]
    fn the_default_is_a_backup_a_day_for_two_weeks() {
        assert_eq!(
            BackupPolicy::default(),
            BackupPolicy::try_new(24, 14).unwrap()
        );
    }

    #[test]
    fn a_name_is_its_second_in_utc() {
        let taken = name("2027-01-15T08:00:42.987Z");

        assert_eq!(taken.file_name(), "afkfleet-backup-20270115T080042Z.db");
        assert_eq!(
            taken.partial_name(),
            ".afkfleet-backup-20270115T080042Z.db.partial"
        );
        assert_eq!(taken.stamp(), at("2027-01-15T08:00:42Z"));
    }

    #[test]
    fn a_name_reads_back_as_itself() {
        let taken = name("2027-12-31T23:59:59Z");

        assert_eq!(BackupName::parse(&taken.file_name()), Some(taken));
        assert_eq!(
            BackupName::parse_partial(&taken.partial_name()),
            Some(taken)
        );
        assert_eq!(
            BackupName::parse_partial_journal(&taken.partial_journal_name()),
            Some(taken)
        );
    }

    #[test]
    fn a_temporary_files_journal_is_named_as_sqlite_names_it() {
        let taken = name("2027-01-15T08:00:42Z");

        assert_eq!(
            taken.partial_journal_name(),
            ".afkfleet-backup-20270115T080042Z.db.partial-journal"
        );
        assert_eq!(
            taken.partial_journal_name(),
            format!("{}-journal", taken.partial_name())
        );
    }

    #[rstest]
    #[case::a_finished_name("afkfleet-backup-20270115T080042Z.db")]
    #[case::a_partial(".afkfleet-backup-20270115T080042Z.db.partial")]
    #[case::no_dot("afkfleet-backup-20270115T080042Z.db.partial-journal")]
    #[case::a_finished_journal(".afkfleet-backup-20270115T080042Z.db-journal")]
    #[case::a_wal(".afkfleet-backup-20270115T080042Z.db.partial-wal")]
    #[case::a_bad_time(".afkfleet-backup-20270115T086042Z.db.partial-journal")]
    fn only_a_temporary_files_journal_counts_as_one(#[case] text: &str) {
        assert_eq!(BackupName::parse_partial_journal(text), None);
    }

    #[rstest]
    #[case::another_prefix("afkfleet-backups-20270115T080042Z.db")]
    #[case::no_prefix("20270115T080042Z.db")]
    #[case::another_suffix("afkfleet-backup-20270115T080042Z.sqlite")]
    #[case::a_partial("afkfleet-backup-20270115T080042Z.db.partial")]
    #[case::a_dot_file(".afkfleet-backup-20270115T080042Z.db")]
    #[case::a_lowercase_zone("afkfleet-backup-20270115T080042z.db")]
    #[case::no_zone("afkfleet-backup-20270115T080042.db")]
    #[case::a_short_date("afkfleet-backup-2027015T080042Z.db")]
    #[case::a_long_year("afkfleet-backup-120270115T080042Z.db")]
    #[case::a_month_13("afkfleet-backup-20271315T080042Z.db")]
    #[case::a_hour_24("afkfleet-backup-20270115T240042Z.db")]
    #[case::a_leap_second("afkfleet-backup-20270115T235960Z.db")]
    #[case::a_sign("afkfleet-backup-+2027115T080042Z.db")]
    #[case::a_space(" afkfleet-backup-20270115T080042Z.db")]
    #[case::a_trailing_part("afkfleet-backup-20270115T080042Z.db.bak")]
    #[case::a_non_ascii_digit("afkfleet-backup-2027011٥T080042Z.db")]
    #[case::empty("")]
    fn only_a_name_the_server_writes_counts(#[case] text: &str) {
        assert_eq!(BackupName::parse(text), None);
    }

    #[rstest]
    #[case::a_finished_name("afkfleet-backup-20270115T080042Z.db")]
    #[case::no_dot("afkfleet-backup-20270115T080042Z.db.partial")]
    #[case::another_suffix(".afkfleet-backup-20270115T080042Z.db.part")]
    #[case::a_bad_time(".afkfleet-backup-20270115T086042Z.db.partial")]
    fn only_a_temporary_name_the_server_writes_counts(#[case] text: &str) {
        assert_eq!(BackupName::parse_partial(text), None);
    }

    #[test]
    fn a_time_past_year_9999_has_no_name() {
        let far = at("9999-12-31T23:59:59Z") + TimeDelta::seconds(1);

        assert_eq!(BackupName::at(far), None);
        assert!(BackupName::at(at("9999-12-31T23:59:59Z")).is_some());
    }

    #[test]
    fn names_sort_in_time_order() {
        let mut names = [
            name("2027-01-15T08:00:00Z"),
            name("2026-12-31T23:59:59Z"),
            name("2027-01-15T07:59:59Z"),
        ];
        names.sort();

        assert_eq!(
            names.map(|name| name.file_name()),
            [
                "afkfleet-backup-20261231T235959Z.db",
                "afkfleet-backup-20270115T075959Z.db",
                "afkfleet-backup-20270115T080000Z.db",
            ]
        );
    }

    #[rstest]
    #[case::none_yet(&[], "2027-01-15T08:00:00Z", true)]
    #[case::one_a_day_old(&["2027-01-14T08:00:00Z"], "2027-01-15T08:00:00Z", true)]
    #[case::one_a_second_younger(&["2027-01-14T08:00:01Z"], "2027-01-15T08:00:00Z", false)]
    #[case::the_newest_counts(
        &["2027-01-10T08:00:00Z", "2027-01-15T07:00:00Z"],
        "2027-01-15T08:00:00Z",
        false
    )]
    #[case::one_dated_just_ahead(&["2027-01-15T09:00:00Z"], "2027-01-15T08:00:00Z", false)]
    #[case::one_dated_far_ahead(&["2027-01-15T09:00:01Z"], "2027-01-15T08:00:00Z", true)]
    fn a_backup_is_due_once_the_newest_is_an_interval_old(
        #[case] finished: &[&str],
        #[case] now: &str,
        #[case] due: bool,
    ) {
        let finished: Vec<BackupName> = finished.iter().map(|text| name(text)).collect();

        assert_eq!(BackupPolicy::default().is_due(&finished, at(now)), due);
    }

    #[test]
    fn pruning_keeps_the_newest_and_goes_newest_first() {
        let policy = BackupPolicy::try_new(24, 2).unwrap();
        let finished = [
            name("2027-01-12T08:00:00Z"),
            name("2027-01-15T08:00:00Z"),
            name("2027-01-13T08:00:00Z"),
            name("2027-01-14T08:00:00Z"),
        ];

        let gone = policy.to_prune(&finished, name("2027-01-15T08:00:00Z"));

        assert_eq!(
            gone,
            [name("2027-01-13T08:00:00Z"), name("2027-01-12T08:00:00Z")]
        );
    }

    #[test]
    fn pruning_never_takes_the_backup_just_written_or_a_newer_one() {
        let policy = BackupPolicy::try_new(24, 1).unwrap();
        // The clock was set back: the new backup is the oldest name.
        let written = name("2027-01-01T08:00:00Z");
        let finished = [name("2027-01-14T08:00:00Z"), written];

        let gone = policy.to_prune(&finished, written);

        assert_eq!(gone, []);
    }

    /// Two processes back up into one folder (`serve` and the command):
    /// each keeps the other's newer backup, which counts toward the kept.
    #[test]
    fn a_newer_backup_is_never_pruned_and_counts_toward_the_kept() {
        let written = name("2027-01-15T08:00:00Z");
        let newer = name("2027-01-15T08:00:05Z");
        let older = name("2027-01-14T08:00:00Z");
        let oldest = name("2027-01-13T08:00:00Z");
        let finished = [oldest, newer, written, older];

        let keep_one = BackupPolicy::try_new(24, 1).unwrap();
        let keep_two = BackupPolicy::try_new(24, 2).unwrap();
        let keep_three = BackupPolicy::try_new(24, 3).unwrap();

        assert_eq!(keep_one.to_prune(&finished, written), [older, oldest]);
        assert_eq!(keep_two.to_prune(&finished, written), [older, oldest]);
        assert_eq!(keep_three.to_prune(&finished, written), [oldest]);
    }

    #[test]
    fn pruning_within_the_count_takes_nothing() {
        let policy = BackupPolicy::default();
        let finished = [name("2027-01-14T08:00:00Z"), name("2027-01-15T08:00:00Z")];

        assert_eq!(policy.to_prune(&finished, name("2027-01-15T08:00:00Z")), []);
    }

    #[rstest]
    #[case::an_hour_old("2027-01-15T07:00:00Z", false)]
    #[case::a_second_short_of_a_day("2027-01-14T08:00:01Z", false)]
    #[case::a_day_old("2027-01-14T08:00:00Z", true)]
    #[case::dated_ahead("2027-01-16T08:00:00Z", false)]
    fn a_leftover_partial_goes_once_a_day_old(#[case] taken: &str, #[case] stale: bool) {
        assert_eq!(
            is_stale_partial(name(taken), at("2027-01-15T08:00:00Z")),
            stale
        );
    }

    #[test]
    fn each_refusal_says_its_bound() {
        assert_eq!(
            BackupPolicyError::Interval.to_string(),
            "the interval must be 1 to 168 hours"
        );
        assert_eq!(
            BackupPolicyError::Keep.to_string(),
            "must keep 1 to 60 backups"
        );
        assert_eq!(
            BackupPolicyError::Span.to_string(),
            "the kept backups times the interval must be at most 720 hours"
        );
    }
}
