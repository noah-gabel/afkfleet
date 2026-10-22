//! `[rate_limits]`: §7.4's request limits (Plan.md P7.10, Appendix A,
//! ADR-0016), the Microsoft link flows' caps (P9.4, ADR-0018), each bot's
//! chat rate, which the server sends in its spec and spends on chat through
//! the API, and each user's (P11.3; ADR-0020, S3), and how many live events
//! sockets a user holds open (P11.5; ADR-0020, U12).

use core::num::NonZeroU32;
use core::time::Duration;

use fleet_core::chat::ChatRate;

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

impl From<ChatRate> for Rate {
    /// A bot's chat rate as a limit: its burst at once, then one line per
    /// interval.
    fn from(rate: ChatRate) -> Self {
        Self {
            interval: rate.interval(),
            burst: rate.burst(),
        }
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
    /// `Connect` calls on the gRPC port, per presented certificate
    /// (`connect_per_cert_per_min`; ADR-0019, S30): spent before any parse
    /// or read.
    pub connect: Rate,
    /// The bot actions that make its account join a server (start, restart,
    /// reset, resume, a new server address), per bot
    /// (`bot_actions_per_bot_per_min`; ADR-0019, S52): spent after
    /// `authorize()`, so a refusal never tells a stranger the bot exists.
    /// Stopping a bot never spends it.
    pub bot_actions: Rate,
    /// Fresh session requests, per bot (`fresh_sessions_per_bot_per_hour`;
    /// ADR-0019, S58): only a request with `fresh` spends it, once the
    /// asking stream is found holding the bot assigned, so an agent never
    /// spends another agent's bots' limit. Each fresh session the server
    /// mints costs a Microsoft refresh, and Microsoft's 429 pauses every
    /// refresh.
    pub fresh_sessions: Rate,
    /// Each bot's chat rate (`chat_per_bot_interval_secs`,
    /// `chat_per_bot_burst`; ADR-0020, S3): sent in its spec, so the
    /// agent's bucket, which mode chat shares, keeps the same rate as the
    /// server's own limit on chat sent through the API.
    pub chat_per_bot: ChatRate,
    /// Chat lines sent through the API, per user (`chat_per_user_per_min`;
    /// ADR-0020, S3): spent after `authorize()` on both chat routes, before
    /// the bot's own rate, so a refusal never tells a stranger the bot
    /// exists.
    pub chat_per_user: Rate,
    /// How many live events sockets one user holds open at once
    /// (`ws_connections_per_user`; ADR-0020, S35, U12): taken before the
    /// upgrade, given back when the socket ends however it ends.
    pub ws_connections_per_user: NonZeroU32,
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
    /// `connect_per_cert_per_min`: 1 to 60. Default 10.
    pub const CONNECT_PER_CERT_PER_MIN: Bounds = Bounds { min: 1, max: 60 };
    /// `bot_actions_per_bot_per_min`: 1 to 60. Default 5.
    pub const BOT_ACTIONS_PER_BOT_PER_MIN: Bounds = Bounds { min: 1, max: 60 };
    /// `fresh_sessions_per_bot_per_hour`: 1 to 60. Default 6.
    pub const FRESH_SESSIONS_PER_BOT_PER_HOUR: Bounds = Bounds { min: 1, max: 60 };
    /// `chat_per_bot_interval_secs`: 1 to 60. Default 3.
    pub const CHAT_PER_BOT_INTERVAL_SECS: Bounds = Bounds {
        min: ChatRate::MIN_INTERVAL_SECS,
        max: ChatRate::MAX_INTERVAL_SECS,
    };
    /// `chat_per_bot_burst`: 1 to 8. Default 3.
    pub const CHAT_PER_BOT_BURST: Bounds = Bounds {
        min: ChatRate::MIN_BURST,
        max: ChatRate::MAX_BURST,
    };
    /// `chat_per_user_per_min`: 1 to 60. Default 20.
    pub const CHAT_PER_USER_PER_MIN: Bounds = Bounds { min: 1, max: 60 };
    /// `ws_connections_per_user`: 1 to 10. Default 3.
    pub const WS_CONNECTIONS_PER_USER: Bounds = Bounds { min: 1, max: 10 };
}

impl Default for RateLimitConfig {
    /// §7.4's and Appendix A's values, group E's limit on grants by
    /// name (ADR-0018), and Phase 10's on enrollments, connects and bot
    /// actions (ADR-0019).
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
            connect: Rate::per_minute(nonzero(10)),
            bot_actions: Rate::per_minute(nonzero(5)),
            fresh_sessions: Rate::per_hour(nonzero(6)),
            chat_per_bot: ChatRate::default(),
            chat_per_user: Rate::per_minute(nonzero(20)),
            ws_connections_per_user: nonzero(3),
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
        assert_eq!(defaults.connect, Rate::per_minute(nonzero(10)));
        assert_eq!(defaults.bot_actions, Rate::per_minute(nonzero(5)));
        assert_eq!(defaults.fresh_sessions, Rate::per_hour(nonzero(6)));
        assert_eq!(defaults.chat_per_bot, ChatRate::default());
        assert_eq!(defaults.chat_per_user, Rate::per_minute(nonzero(20)));
        assert_eq!(defaults.ws_connections_per_user.get(), 3);
    }

    #[test]
    fn a_chat_rate_is_its_burst_then_one_line_per_interval() {
        let rate = Rate::from(ChatRate::try_new(5, 2).unwrap());

        assert_eq!(rate.interval(), Duration::from_secs(5));
        assert_eq!(rate.burst().get(), 2);
    }

    #[test]
    fn a_zero_falls_back_to_one() {
        assert_eq!(nonzero(0).get(), 1);
        assert_eq!(nonzero(7).get(), 7);
    }
}
