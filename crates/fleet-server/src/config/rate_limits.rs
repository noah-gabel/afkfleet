//! `[rate_limits]`: §7.4's request limits (Plan.md P7.10, Appendix A,
//! ADR-0016), and the Microsoft link flows' caps (P9.4, ADR-0018). The chat
//! keys and `ws_connections_per_user` arrive with P11.

use core::num::NonZeroU32;
use core::time::Duration;

use super::Bounds;

/// One limit: `burst` requests at once, refilled at one per `interval`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    interval: Duration,
    burst: NonZeroU32,
}

impl Rate {
    /// `count` requests a second, `burst` at once.
    #[must_use]
    pub const fn per_second(count: NonZeroU32, burst: NonZeroU32) -> Self {
        Self::spread(Duration::from_secs(1), count, burst)
    }

    /// `count` requests a minute, all of them at once if need be.
    #[must_use]
    pub const fn per_minute(count: NonZeroU32) -> Self {
        Self::spread(Duration::from_mins(1), count, count)
    }

    /// `count` requests an hour, all of them at once if need be.
    #[must_use]
    pub const fn per_hour(count: NonZeroU32) -> Self {
        Self::spread(Duration::from_hours(1), count, count)
    }

    /// `count` requests per `period`, `burst` at once.
    const fn spread(period: Duration, count: NonZeroU32, burst: NonZeroU32) -> Self {
        Self {
            // A count is never 0, so the division always succeeds.
            interval: match period.checked_div(count.get()) {
                Some(interval) => interval,
                None => period,
            },
            burst,
        }
    }

    /// How long one more request takes to become allowed again.
    #[must_use]
    pub const fn interval(&self) -> Duration {
        self.interval
    }

    /// How many requests are allowed at once.
    #[must_use]
    pub const fn burst(&self) -> NonZeroU32 {
        self.burst
    }
}

/// `[rate_limits]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitConfig {
    /// Every request, per client IP (`per_ip_per_sec`, `per_ip_burst`).
    pub per_ip: Rate,
    /// `POST /auth/login` and `/auth/login/mfa` together, per client IP
    /// (`login_per_ip_per_min`).
    pub login: Rate,
    /// `POST /auth/refresh`, per client IP (`refresh_per_ip_per_min`).
    pub refresh: Rate,
    /// `POST /auth/register`, per client IP (`register_per_ip_per_hour`).
    pub register: Rate,
    /// Every authed request, per user (`per_user_per_sec`,
    /// `per_user_burst`).
    pub per_user: Rate,
    /// The Microsoft link flows (`link_flows_total`,
    /// `link_flows_per_user_per_day`).
    pub link_flows: LinkFlowLimits,
    /// Grants created by a username, per user
    /// (`grants_by_name_per_user_per_hour`): each one tries a name, so this
    /// bounds how many names one user can probe (ADR-0018, group E).
    pub grants_by_name: Rate,
    /// `Enroll` calls on the gRPC port, per client IP
    /// (`enroll_per_ip_per_min`; ADR-0019, S14): spent before the request
    /// is read.
    pub enroll: Rate,
}

/// `[rate_limits]`'s caps on Microsoft link flows (P9.4, ADR-0018). Each
/// user runs one flow at a time, a fixed rule: a new start replaces their
/// waiting flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkFlowLimits {
    /// How many flows run at once on the whole server
    /// (`link_flows_total`); one more start is refused as busy.
    pub total: NonZeroU32,
    /// How many flows one user starts in any 24 hours
    /// (`link_flows_per_user_per_day`), counted from the audit log.
    pub per_user_per_day: NonZeroU32,
}

impl Default for LinkFlowLimits {
    /// §7.4's: 3 at once, 10 a day per user.
    fn default() -> Self {
        Self {
            total: nonzero(3),
            per_user_per_day: nonzero(10),
        }
    }
}

impl RateLimitConfig {
    /// `per_ip_per_sec`: 1 to 1000. Default 20.
    pub const PER_IP_PER_SEC: Bounds = Bounds { min: 1, max: 1000 };
    /// `per_ip_burst`: 1 to 10000. Default 40.
    pub const PER_IP_BURST: Bounds = Bounds {
        min: 1,
        max: 10_000,
    };
    /// `login_per_ip_per_min`: 1 to 60. Default 5.
    pub const LOGIN_PER_IP_PER_MIN: Bounds = Bounds { min: 1, max: 60 };
    /// `refresh_per_ip_per_min`: 1 to 600. Default 30.
    pub const REFRESH_PER_IP_PER_MIN: Bounds = Bounds { min: 1, max: 600 };
    /// `register_per_ip_per_hour`: 1 to 100. Default 5.
    pub const REGISTER_PER_IP_PER_HOUR: Bounds = Bounds { min: 1, max: 100 };
    /// `per_user_per_sec`: 1 to 1000. Default 10.
    pub const PER_USER_PER_SEC: Bounds = Bounds { min: 1, max: 1000 };
    /// `per_user_burst`: 1 to 10000. Default 30.
    pub const PER_USER_BURST: Bounds = Bounds {
        min: 1,
        max: 10_000,
    };
    /// `link_flows_total`: 1 to 16. Default 3.
    pub const LINK_FLOWS_TOTAL: Bounds = Bounds { min: 1, max: 16 };
    /// `link_flows_per_user_per_day`: 1 to 100. Default 10.
    pub const LINK_FLOWS_PER_USER_PER_DAY: Bounds = Bounds { min: 1, max: 100 };
    /// `grants_by_name_per_user_per_hour`: 1 to 100. Default 10.
    pub const GRANTS_BY_NAME_PER_USER_PER_HOUR: Bounds = Bounds { min: 1, max: 100 };
    /// `enroll_per_ip_per_min`: 1 to 60. Default 5.
    pub const ENROLL_PER_IP_PER_MIN: Bounds = Bounds { min: 1, max: 60 };
}

impl Default for RateLimitConfig {
    /// §7.4's and Appendix A's values, group E's limit on grants by
    /// name (ADR-0018), and group B's on enrollments (ADR-0019).
    fn default() -> Self {
        Self {
            per_ip: Rate::per_second(nonzero(20), nonzero(40)),
            login: Rate::per_minute(nonzero(5)),
            refresh: Rate::per_minute(nonzero(30)),
            register: Rate::per_hour(nonzero(5)),
            per_user: Rate::per_second(nonzero(10), nonzero(30)),
            link_flows: LinkFlowLimits::default(),
            grants_by_name: Rate::per_hour(nonzero(10)),
            enroll: Rate::per_minute(nonzero(5)),
        }
    }
}

/// `n` as a `NonZeroU32`, or 1 for 0, which no default is.
pub(super) const fn nonzero(n: u32) -> NonZeroU32 {
    match NonZeroU32::new(n) {
        Some(n) => n,
        None => NonZeroU32::MIN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_spreads_its_count_over_its_period() {
        let rate = Rate::per_second(nonzero(20), nonzero(40));

        assert_eq!(rate.interval(), Duration::from_millis(50));
        assert_eq!(rate.burst().get(), 40);
    }

    #[test]
    fn a_rate_per_minute_or_hour_allows_its_whole_count_at_once() {
        let login = Rate::per_minute(nonzero(5));
        let register = Rate::per_hour(nonzero(5));

        assert_eq!(login.interval(), Duration::from_secs(12));
        assert_eq!(login.burst().get(), 5);
        assert_eq!(register.interval(), Duration::from_mins(12));
        assert_eq!(register.burst().get(), 5);
    }

    #[test]
    fn the_defaults_are_appendix_as() {
        let defaults = RateLimitConfig::default();

        assert_eq!(defaults.per_ip, Rate::per_second(nonzero(20), nonzero(40)));
        assert_eq!(defaults.login, Rate::per_minute(nonzero(5)));
        assert_eq!(defaults.refresh, Rate::per_minute(nonzero(30)));
        assert_eq!(defaults.register, Rate::per_hour(nonzero(5)));
        assert_eq!(
            defaults.per_user,
            Rate::per_second(nonzero(10), nonzero(30))
        );
        assert_eq!(defaults.link_flows.total.get(), 3);
        assert_eq!(defaults.link_flows.per_user_per_day.get(), 10);
        assert_eq!(defaults.grants_by_name, Rate::per_hour(nonzero(10)));
        assert_eq!(defaults.enroll, Rate::per_minute(nonzero(5)));
    }

    #[test]
    fn a_zero_falls_back_to_one() {
        assert_eq!(nonzero(0).get(), 1);
        assert_eq!(nonzero(7).get(), 7);
    }
}
