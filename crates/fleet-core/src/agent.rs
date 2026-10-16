//! An agent's admission to the control plane (Plan.md P10.4; ADR-0019, S1,
//! S24, S27, S30): the rules the server applies to the certificate an agent
//! connects with, and how many of its refusals are audited.
//!
//! - **A certificate admits its agent** while its row is unrevoked and
//!   unexpired and its agent enabled. [`admit`] checks them in that order,
//!   so a disabled agent's certificate, which disabling revokes, is refused
//!   as revoked. The SAN's agent is the caller's to compare: it needs the
//!   certificate itself.
//! - **Expired** means from `not_after` on ([`certificate_expired`]), as the
//!   `agent_certs` table counts it: one second stricter than rustls, which
//!   still accepts a certificate at exactly its `not_after`.
//! - **Refusals** of a certificate the server knows are audited, at most
//!   [`MAX_AUDITED_CONNECT_REFUSALS`] per agent in any
//!   [`CONNECT_REFUSAL_WINDOW`]; later ones are only logged.
//! - **An agent's status** as the agents' list shows it ([`status`],
//!   P10.10, S31): disabled, else expired when no unrevoked certificate is
//!   valid, else online while its stream lives, else offline.

use core::fmt;
use core::time::Duration;

use chrono::{DateTime, Utc};

/// How many refused connections of one agent are audited in any
/// [`CONNECT_REFUSAL_WINDOW`]; later ones are only logged.
pub const MAX_AUDITED_CONNECT_REFUSALS: u32 = 10;

/// The rolling window [`MAX_AUDITED_CONNECT_REFUSALS`] counts in.
pub const CONNECT_REFUSAL_WINDOW: Duration = Duration::from_hours(24);

/// Why a certificate the server knows doesn't admit its agent. Its
/// [`as_str`](Self::as_str) is the audit entry's `reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectRefusal {
    /// The certificate was revoked.
    Revoked,
    /// The certificate expired.
    Expired,
    /// The agent is disabled.
    Disabled,
    /// The certificate's SAN doesn't name its row's agent, or doesn't read.
    SanMismatch,
    /// The hello names another agent than the certificate's.
    NameMismatch,
}

impl ConnectRefusal {
    /// The reason's stable name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Revoked => "revoked",
            Self::Expired => "expired",
            Self::Disabled => "disabled",
            Self::SanMismatch => "san_mismatch",
            Self::NameMismatch => "name_mismatch",
        }
    }
}

impl fmt::Display for ConnectRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the server stored about a certificate and its agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CertificateFacts {
    /// Whether the certificate was revoked.
    pub revoked: bool,
    /// The certificate's `notAfter`.
    pub not_after: DateTime<Utc>,
    /// Whether its agent is enabled.
    pub agent_enabled: bool,
}

/// Whether a certificate valid until `not_after` has expired at `now`: from
/// `not_after` on.
#[must_use]
pub fn certificate_expired(not_after: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    not_after <= now
}

/// How long a certificate valid until `not_after` has left at `now`; zero
/// once it has expired. It never overflows, however far `not_after` lies.
#[must_use]
pub fn certificate_time_left(not_after: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    (not_after - now).to_std().unwrap_or(Duration::ZERO)
}

/// Whether `facts` admit the certificate's agent at `now`; see the
/// [module docs](self).
///
/// # Errors
/// [`ConnectRefusal::Revoked`], [`ConnectRefusal::Expired`] or
/// [`ConnectRefusal::Disabled`], the first that applies in that order.
pub fn admit(facts: CertificateFacts, now: DateTime<Utc>) -> Result<(), ConnectRefusal> {
    if facts.revoked {
        Err(ConnectRefusal::Revoked)
    } else if certificate_expired(facts.not_after, now) {
        Err(ConnectRefusal::Expired)
    } else if !facts.agent_enabled {
        Err(ConnectRefusal::Disabled)
    } else {
        Ok(())
    }
}

/// An agent's status as the agents' list shows it (P10.10; ADR-0019, S31).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentStatus {
    /// Enabled, with a valid certificate and a live stream.
    Online,
    /// Enabled, with a valid certificate, and no live stream.
    Offline,
    /// Enabled, but no unrevoked certificate is valid: it can't connect
    /// until it enrolls again.
    Expired,
    /// Disabled: every certificate it had is revoked.
    Disabled,
}

/// The status of an agent enabled or not as `enabled` says, whose latest
/// unrevoked certificate expires at `not_after` (none: it has none), with a
/// live stream or not, at `now`.
#[must_use]
pub fn status(
    enabled: bool,
    not_after: Option<DateTime<Utc>>,
    live: bool,
    now: DateTime<Utc>,
) -> AgentStatus {
    if !enabled {
        return AgentStatus::Disabled;
    }
    if not_after.is_none_or(|not_after| certificate_expired(not_after, now)) {
        return AgentStatus::Expired;
    }
    if live {
        AgentStatus::Online
    } else {
        AgentStatus::Offline
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rstest::rstest;

    use super::*;

    fn at(millis: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(millis).unwrap()
    }

    const NOW: i64 = 1_790_000_000_000;

    fn facts(revoked: bool, not_after: i64, agent_enabled: bool) -> CertificateFacts {
        CertificateFacts {
            revoked,
            not_after: at(not_after),
            agent_enabled,
        }
    }

    #[rstest]
    #[case::a_millisecond_before(NOW + 1, false)]
    #[case::at_its_not_after(NOW, true)]
    #[case::a_second_after(NOW - 1000, true)]
    fn a_certificate_is_expired_from_its_not_after_on(
        #[case] not_after: i64,
        #[case] expired: bool,
    ) {
        assert_eq!(certificate_expired(at(not_after), at(NOW)), expired);
    }

    #[rstest]
    #[case::valid(false, NOW + 1, true, Ok(()))]
    #[case::revoked(true, NOW + 1, true, Err(ConnectRefusal::Revoked))]
    #[case::expired(false, NOW, true, Err(ConnectRefusal::Expired))]
    #[case::disabled(false, NOW + 1, false, Err(ConnectRefusal::Disabled))]
    #[case::revoked_before_expired(true, NOW, true, Err(ConnectRefusal::Revoked))]
    #[case::a_disabled_agents_revoked_certificate_is_revoked(true, NOW + 1, false, Err(ConnectRefusal::Revoked))]
    #[case::expired_before_disabled(false, NOW, false, Err(ConnectRefusal::Expired))]
    fn admission_checks_revoked_then_expired_then_disabled(
        #[case] revoked: bool,
        #[case] not_after: i64,
        #[case] enabled: bool,
        #[case] expected: Result<(), ConnectRefusal>,
    ) {
        assert_eq!(admit(facts(revoked, not_after, enabled), at(NOW)), expected);
    }

    #[test]
    fn every_refusal_has_its_stable_name() {
        let names = [
            ConnectRefusal::Revoked,
            ConnectRefusal::Expired,
            ConnectRefusal::Disabled,
            ConnectRefusal::SanMismatch,
            ConnectRefusal::NameMismatch,
        ]
        .map(|refusal| refusal.to_string());

        assert_eq!(
            names,
            [
                "revoked",
                "expired",
                "disabled",
                "san_mismatch",
                "name_mismatch"
            ]
        );
    }

    #[test]
    fn ten_refusals_a_day_are_audited() {
        assert_eq!(MAX_AUDITED_CONNECT_REFUSALS, 10);
        assert_eq!(CONNECT_REFUSAL_WINDOW, Duration::from_hours(24));
    }

    #[test]
    fn the_time_left_runs_to_the_not_after() {
        assert_eq!(
            certificate_time_left(at(NOW + 1500), at(NOW)),
            Duration::from_millis(1500)
        );
        assert_eq!(certificate_time_left(at(NOW), at(NOW)), Duration::ZERO);
    }

    proptest! {
        #[test]
        fn the_time_left_is_never_negative_and_a_far_not_after_never_overflows(
            not_after in prop_oneof![
                Just(DateTime::<Utc>::MAX_UTC),
                Just(DateTime::<Utc>::MIN_UTC),
                (NOW - 10_000..NOW + 10_000).prop_map(at),
                (DateTime::<Utc>::MIN_UTC.timestamp_millis()..=DateTime::<Utc>::MAX_UTC.timestamp_millis())
                    .prop_filter_map("in chrono's range", DateTime::from_timestamp_millis),
            ],
        ) {
            let left = certificate_time_left(not_after, at(NOW));

            prop_assert_eq!(left == Duration::ZERO, certificate_expired(not_after, at(NOW)));
        }
    }

    #[rstest]
    #[case::disabled_wins(false, Some(NOW + 1), true, AgentStatus::Disabled)]
    #[case::disabled_without_a_certificate(false, None, false, AgentStatus::Disabled)]
    #[case::no_certificate(true, None, false, AgentStatus::Expired)]
    #[case::expired_at_its_not_after(true, Some(NOW), true, AgentStatus::Expired)]
    #[case::valid_and_live(true, Some(NOW + 1), true, AgentStatus::Online)]
    #[case::valid_and_quiet(true, Some(NOW + 1), false, AgentStatus::Offline)]
    fn an_agents_status_is_disabled_then_expired_then_live_or_not(
        #[case] enabled: bool,
        #[case] not_after: Option<i64>,
        #[case] live: bool,
        #[case] expected: AgentStatus,
    ) {
        assert_eq!(status(enabled, not_after.map(at), live, at(NOW)), expected);
    }
}
