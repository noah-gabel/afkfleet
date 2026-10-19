//! Rate limits (Plan.md P7.10, §7.4, ADR-0016): §7.4's request limits, each
//! a keyed governor limiter on an injected clock.
//!
//! - **Every request spends its client's per-IP limit**, in the
//!   middleware stack, before the timeout, the body or the router, so a 404
//!   scan and the health checks count too. The layer resolves the client's
//!   address once ([`client_ip`]) and puts it into the request as
//!   [`ClientIp`].
//! - **A route spends its class's limit** ([`RateClass`]): the login routes
//!   share one per-IP bucket, refresh and register have their own, and
//!   `Standard` spends nothing more. It's a route layer, so a
//!   404 or 405 spends no class token.
//! - **Every authed request spends its user's limit,** in `AuthUser`, right
//!   after authentication ([`RateLimits::check_user`]).
//! - **Creating a grant by a username spends its own per-user limit** too,
//!   in its handler after the step-up check, since each one tries a name
//!   ([`RateLimits::check_grant_by_name`]; ADR-0018, group E).
//! - **An `Enroll` call on the gRPC port spends its client's own per-IP
//!   limit,** in that port's interceptor before the request is read
//!   ([`RateLimits::check_enroll`]; ADR-0019, S14).
//! - **A `Connect` call spends its client certificate's limit,** in that
//!   port's other interceptor, right after the certificate check and before
//!   any parse or read ([`RateLimits::check_connect`]; ADR-0019, S30).
//! - **A bot action that makes its account join a server spends the bot's
//!   limit** (start, restart, reset, resume, a new server address), in its
//!   handler right after `authorize()`, so it never tells a stranger the bot
//!   exists; stopping never spends it ([`RateLimits::check_bot_action`];
//!   ADR-0019, S52).
//! - **A fresh session request on the gRPC port spends its bot's limit**,
//!   in its stream's session, once the registry found the stream holding the
//!   bot assigned, and before the request is queued: each fresh session the
//!   server mints costs a Microsoft refresh
//!   ([`RateLimits::check_fresh_session`]; ADR-0019, S58).
//! - **Keys:** an IPv4 client by its address, an IPv6 client by its /64
//!   ([`limiter_key`]); a user by their ID; a client certificate by its
//!   fingerprint; a bot by its ID.
//! - **A refusal** is `ApiError::RateLimited` with governor's wait, which
//!   the error model sends as `Retry-After` in whole seconds, rounded up.
//!   The trace layer logs a refused request's line at `debug`, and each
//!   limit counts its refusals ([`RateLimits::take_refusals`]) for the
//!   server's periodic summary, so a flood doesn't flood the log.
//! - **The clock** is the server's `Clock` port ([`LimiterClock`]): in
//!   production a monotonic one, so a step of the wall clock can't refuse
//!   every client, and in tests the `ManualClock` everything else uses.
//! - **Memory:** a limiter keeps one entry per key it has seen;
//!   [`RateLimits::retain_recent`], run periodically by the server, drops
//!   every entry whose bucket has been full again for one interval.

mod client_ip;
mod net;

use core::fmt;
use core::num::NonZeroU32;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, FromRequestParts as _, Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use chrono::{DateTime, Utc};
use fleet_core::id::{BotId, UserId};
use fleet_core::pki::CertFingerprint;
use fleet_core::system::Clock;
use fleet_core::time::elapsed;
use governor::middleware::NoOpMiddleware;
use governor::nanos::Nanos;
use governor::state::keyed::HashMapStateStore;
use governor::{Quota, RateLimiter};

pub use client_ip::{ClientIp, MissingClientIp, MissingPeerAddress, X_FORWARDED_FOR, client_ip};
pub use net::{IpNet, IpNetError, TrustedProxies, limiter_key};

use super::error::ApiError;
use super::routes::RateClass;
use crate::config::{Rate, RateLimitConfig};

/// governor's clock over the server's [`Clock`] port: the time since its
/// first reading. A reading before that (a clock that went back) counts as
/// that first reading, so it never panics; production passes a monotonic
/// clock, which never goes back.
#[derive(Clone)]
pub struct LimiterClock {
    clock: Arc<dyn Clock>,
    start: DateTime<Utc>,
}

impl LimiterClock {
    /// A clock that reads `clock`, starting now.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        let start = clock.now();
        Self { clock, start }
    }
}

impl fmt::Debug for LimiterClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LimiterClock")
            .field("start", &self.start)
            .finish_non_exhaustive()
    }
}

impl governor::clock::Clock for LimiterClock {
    type Instant = Nanos;

    fn now(&self) -> Nanos {
        Nanos::from(elapsed(self.start, self.clock.now()))
    }
}

/// A keyed limiter on the [`LimiterClock`].
type Limiter<K> = RateLimiter<K, HashMapStateStore<K>, LimiterClock, NoOpMiddleware<Nanos>>;

/// A refused request: when its client may try again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused {
    /// The limiter's wait until one more request is allowed.
    pub retry_after: Duration,
}

/// `rate_limited`, with the wait as `Retry-After`.
impl From<Refused> for ApiError {
    fn from(refused: Refused) -> Self {
        Self::RateLimited {
            retry_after: refused.retry_after,
        }
    }
}

/// How many requests each limit refused since the last
/// [`RateLimits::take_refusals`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Refusals {
    /// The global per-IP limit.
    pub per_ip: u64,
    /// The login routes' shared per-IP limit.
    pub login: u64,
    /// The refresh route's per-IP limit.
    pub refresh: u64,
    /// The register route's per-IP limit.
    pub register: u64,
    /// The per-user limit of authed requests.
    pub per_user: u64,
    /// The per-user limit of grants created by a username.
    pub grants_by_name: u64,
    /// The per-IP limit of `Enroll` calls on the gRPC port.
    pub enroll: u64,
    /// The per-certificate limit of `Connect` calls on the gRPC port.
    pub connect: u64,
    /// The per-bot limit of actions that make its account join a server.
    pub bot_actions: u64,
    /// The per-bot limit of fresh session requests on the gRPC port.
    pub fresh_sessions: u64,
}

impl Refusals {
    /// Whether no limit refused anything.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.per_ip == 0
            && self.login == 0
            && self.refresh == 0
            && self.register == 0
            && self.per_user == 0
            && self.grants_by_name == 0
            && self.enroll == 0
            && self.connect == 0
            && self.bot_actions == 0
            && self.fresh_sessions == 0
    }
}

/// The refusal counters, one per limit.
#[derive(Debug, Default)]
struct Counters {
    per_ip: AtomicU64,
    login: AtomicU64,
    refresh: AtomicU64,
    register: AtomicU64,
    per_user: AtomicU64,
    grants_by_name: AtomicU64,
    enroll: AtomicU64,
    connect: AtomicU64,
    bot_actions: AtomicU64,
    fresh_sessions: AtomicU64,
}

/// Every limiter, built from `[rate_limits]`.
struct Limiters {
    clock: LimiterClock,
    per_ip: Limiter<IpAddr>,
    login: Limiter<IpAddr>,
    refresh: Limiter<IpAddr>,
    register: Limiter<IpAddr>,
    per_user: Limiter<UserId>,
    grants_by_name: Limiter<UserId>,
    enroll: Limiter<IpAddr>,
    connect: Limiter<CertFingerprint>,
    bot_actions: Limiter<BotId>,
    fresh_sessions: Limiter<BotId>,
    refused: Counters,
}

/// §7.4's request limits; clones share them. See the [module docs](self).
#[derive(Clone)]
pub struct RateLimits(Arc<Limiters>);

impl fmt::Debug for RateLimits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RateLimits")
            .field("clock", &self.0.clock)
            .field("tracked", &self.tracked())
            .finish_non_exhaustive()
    }
}

/// The limiter for `rate`. A zero interval can't come from the config's
/// ranges; it would allow everything at once instead of panicking.
fn limiter<K: core::hash::Hash + Eq + Clone>(rate: Rate, clock: &LimiterClock) -> Limiter<K> {
    let quota = Quota::with_period(rate.interval())
        .unwrap_or_else(|| Quota::per_second(NonZeroU32::MAX))
        .allow_burst(rate.burst());
    RateLimiter::hashmap_with_clock(quota, clock.clone())
}

impl RateLimits {
    /// The limits of `config`, on `clock`.
    #[must_use]
    pub fn new(config: &RateLimitConfig, clock: Arc<dyn Clock>) -> Self {
        let clock = LimiterClock::new(clock);
        Self(Arc::new(Limiters {
            per_ip: limiter(config.per_ip, &clock),
            login: limiter(config.login, &clock),
            refresh: limiter(config.refresh, &clock),
            register: limiter(config.register, &clock),
            per_user: limiter(config.per_user, &clock),
            grants_by_name: limiter(config.grants_by_name, &clock),
            enroll: limiter(config.enroll, &clock),
            connect: limiter(config.connect, &clock),
            bot_actions: limiter(config.bot_actions, &clock),
            fresh_sessions: limiter(config.fresh_sessions, &clock),
            refused: Counters::default(),
            clock,
        }))
    }

    /// Spends one of `ip`'s global per-IP requests.
    ///
    /// # Errors
    /// [`Refused`] when its bucket is empty.
    pub fn check_ip(&self, ip: IpAddr) -> Result<(), Refused> {
        let limits = &self.0;
        spend(
            &limits.per_ip,
            &limits.clock,
            &limiter_key(ip),
            &limits.refused.per_ip,
        )
    }

    /// Spends one of `ip`'s requests of `class`; `Standard` spends nothing.
    ///
    /// # Errors
    /// [`Refused`] when its bucket is empty.
    pub fn check_class(&self, class: RateClass, ip: IpAddr) -> Result<(), Refused> {
        let limits = &self.0;
        let (limiter, refused) = match class {
            RateClass::Standard => return Ok(()),
            RateClass::Login => (&limits.login, &limits.refused.login),
            RateClass::Refresh => (&limits.refresh, &limits.refused.refresh),
            RateClass::Register => (&limits.register, &limits.refused.register),
        };
        spend(limiter, &limits.clock, &limiter_key(ip), refused)
    }

    /// Spends one of `user`'s grants created by a username, which the grant
    /// handler does right after the step-up check: each one tries a name
    /// (ADR-0018, group E).
    ///
    /// # Errors
    /// [`Refused`] when their bucket is empty.
    pub fn check_grant_by_name(&self, user: UserId) -> Result<(), Refused> {
        let limits = &self.0;
        spend(
            &limits.grants_by_name,
            &limits.clock,
            &user,
            &limits.refused.grants_by_name,
        )
    }

    /// Spends one of `ip`'s `Enroll` calls, which the gRPC port's
    /// interceptor does before the request is read (ADR-0019, S14).
    ///
    /// # Errors
    /// [`Refused`] when its bucket is empty.
    pub fn check_enroll(&self, ip: IpAddr) -> Result<(), Refused> {
        let limits = &self.0;
        spend(
            &limits.enroll,
            &limits.clock,
            &limiter_key(ip),
            &limits.refused.enroll,
        )
    }

    /// Spends one of the `Connect` calls of the client certificate whose
    /// fingerprint is `certificate`, which the gRPC port's gate does before
    /// any parse or read (ADR-0019, S30).
    ///
    /// # Errors
    /// [`Refused`] when its bucket is empty.
    pub fn check_connect(&self, certificate: CertFingerprint) -> Result<(), Refused> {
        let limits = &self.0;
        spend(
            &limits.connect,
            &limits.clock,
            &certificate,
            &limits.refused.connect,
        )
    }

    /// Spends one of `bot`'s actions that make its account join a server,
    /// which its handler does right after `authorize()` (ADR-0019, S52).
    ///
    /// # Errors
    /// [`Refused`] when its bucket is empty.
    pub fn check_bot_action(&self, bot: BotId) -> Result<(), Refused> {
        let limits = &self.0;
        spend(
            &limits.bot_actions,
            &limits.clock,
            &bot,
            &limits.refused.bot_actions,
        )
    }

    /// Spends one of `bot`'s fresh session requests, which a `Connect`
    /// stream does once it found itself holding the bot assigned, before
    /// the request is queued (ADR-0019, S58).
    ///
    /// # Errors
    /// [`Refused`] when its bucket is empty.
    pub fn check_fresh_session(&self, bot: BotId) -> Result<(), Refused> {
        let limits = &self.0;
        spend(
            &limits.fresh_sessions,
            &limits.clock,
            &bot,
            &limits.refused.fresh_sessions,
        )
    }

    /// Spends one of `user`'s authed requests.
    ///
    /// # Errors
    /// [`Refused`] when their bucket is empty.
    pub fn check_user(&self, user: UserId) -> Result<(), Refused> {
        let limits = &self.0;
        spend(
            &limits.per_user,
            &limits.clock,
            &user,
            &limits.refused.per_user,
        )
    }

    /// Drops every key whose bucket has been full again for one interval:
    /// governor keeps one entry per key it has seen until then.
    pub fn retain_recent(&self) {
        let limits = &self.0;
        limits.per_ip.retain_recent();
        limits.login.retain_recent();
        limits.refresh.retain_recent();
        limits.register.retain_recent();
        limits.per_user.retain_recent();
        limits.grants_by_name.retain_recent();
        limits.enroll.retain_recent();
        limits.connect.retain_recent();
        limits.bot_actions.retain_recent();
        limits.fresh_sessions.retain_recent();
    }

    /// How many keys the limiters hold.
    #[must_use]
    pub fn tracked(&self) -> usize {
        let limits = &self.0;
        limits.per_ip.len()
            + limits.login.len()
            + limits.refresh.len()
            + limits.register.len()
            + limits.per_user.len()
            + limits.grants_by_name.len()
            + limits.enroll.len()
            + limits.connect.len()
            + limits.bot_actions.len()
            + limits.fresh_sessions.len()
    }

    /// The refusals since the last call, and starts counting again.
    #[must_use]
    pub fn take_refusals(&self) -> Refusals {
        let refused = &self.0.refused;
        let take = |counter: &AtomicU64| counter.swap(0, Ordering::Relaxed);
        Refusals {
            per_ip: take(&refused.per_ip),
            login: take(&refused.login),
            refresh: take(&refused.refresh),
            register: take(&refused.register),
            per_user: take(&refused.per_user),
            grants_by_name: take(&refused.grants_by_name),
            enroll: take(&refused.enroll),
            connect: take(&refused.connect),
            bot_actions: take(&refused.bot_actions),
            fresh_sessions: take(&refused.fresh_sessions),
        }
    }
}

/// What the stack's per-IP layer needs: the trusted proxies and the limits.
#[derive(Debug, Clone)]
pub(crate) struct PerIpLayer {
    proxies: Arc<TrustedProxies>,
    limits: RateLimits,
}

impl PerIpLayer {
    /// The layer's state over `proxies` and `limits`.
    pub(crate) fn new(proxies: &TrustedProxies, limits: RateLimits) -> Self {
        Self {
            proxies: Arc::new(proxies.clone()),
            limits,
        }
    }
}

/// The stack's per-IP layer: resolves the client's address, puts it into
/// the request as [`ClientIp`] and spends one of its per-IP requests. The
/// peer comes from axum's `ConnectInfo` extractor (which a test's
/// `MockConnectInfo` feeds too); a request without it is a wiring bug: an
/// internal error.
pub(crate) async fn per_ip(
    State(layer): State<PerIpLayer>,
    request: Request,
    next: Next,
) -> Response {
    let (mut parts, body) = request.into_parts();
    let Ok(ConnectInfo(peer)) =
        ConnectInfo::<SocketAddr>::from_request_parts(&mut parts, &()).await
    else {
        return ApiError::internal(MissingPeerAddress).into_response();
    };
    let client = client_ip(peer.ip(), &parts.headers, &layer.proxies);
    parts.extensions.insert(ClientIp(client));
    let request = Request::from_parts(parts, body);
    if let Err(refused) = layer.limits.check_ip(client) {
        return ApiError::from(refused).into_response();
    }
    next.run(request).await
}

/// What a route's class layer needs.
#[derive(Debug, Clone)]
pub(crate) struct ClassLayer {
    class: RateClass,
    limits: RateLimits,
}

impl ClassLayer {
    /// The layer's state for `class` over `limits`.
    pub(crate) const fn new(class: RateClass, limits: RateLimits) -> Self {
        Self { class, limits }
    }
}

/// A route's class layer: spends one of the client's requests of the
/// route's class. The client's address comes from the stack's per-IP
/// layer; without it, the route was served outside the stack, a wiring bug.
pub(crate) async fn per_class(
    State(layer): State<ClassLayer>,
    request: Request,
    next: Next,
) -> Response {
    let Some(ClientIp(client)) = request.extensions().get::<ClientIp>().copied() else {
        return ApiError::internal(MissingClientIp).into_response();
    };
    if let Err(refused) = layer.limits.check_class(layer.class, client) {
        return ApiError::from(refused).into_response();
    }
    next.run(request).await
}

/// Takes one key's request from `limiter`, counting a refusal in `refused`.
fn spend<K: core::hash::Hash + Eq + Clone>(
    limiter: &Limiter<K>,
    clock: &LimiterClock,
    key: &K,
    refused: &AtomicU64,
) -> Result<(), Refused> {
    use governor::clock::Clock as _;

    limiter.check_key(key).map_err(|not_until| {
        refused.fetch_add(1, Ordering::Relaxed);
        Refused {
            retry_after: not_until.wait_time_from(clock.now()),
        }
    })
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;
    use fleet_testkit::system::ManualClock;
    use governor::clock::Clock as _;

    use super::*;
    use crate::config::LinkFlowLimits;

    fn start() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2027, 1, 15, 8, 0, 0).unwrap()
    }

    fn nonzero(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).unwrap()
    }

    /// Two requests a second with a burst of 2 for every limit.
    fn tight() -> RateLimitConfig {
        let rate = Rate::per_second(nonzero(2), nonzero(2));
        RateLimitConfig {
            per_ip: rate,
            login: rate,
            refresh: rate,
            register: rate,
            per_user: rate,
            link_flows: LinkFlowLimits::default(),
            grants_by_name: rate,
            enroll: rate,
            connect: rate,
            bot_actions: rate,
            fresh_sessions: rate,
            chat_per_bot: fleet_core::chat::ChatRate::default(),
        }
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn user(n: u8) -> UserId {
        UserId::new_v7(start(), [n; 10]).unwrap()
    }

    #[test]
    fn the_limiter_clock_measures_from_its_first_reading() {
        let clock = ManualClock::new(start());
        let limiter = LimiterClock::new(Arc::new(clock.clone()));

        clock.advance(Duration::from_millis(1500));

        assert_eq!(Duration::from(limiter.now()), Duration::from_millis(1500));
    }

    #[test]
    fn a_clock_that_went_back_reads_as_the_first_reading() {
        let clock = ManualClock::new(start());
        let limiter = LimiterClock::new(Arc::new(clock.clone()));

        clock.set(start() - chrono::TimeDelta::seconds(30));

        assert_eq!(Duration::from(limiter.now()), Duration::ZERO);
    }

    #[test]
    fn a_refusal_is_rate_limited_with_its_wait() {
        let error = ApiError::from(Refused {
            retry_after: Duration::from_millis(1200),
        });

        assert!(matches!(
            error,
            ApiError::RateLimited { retry_after } if retry_after == Duration::from_millis(1200)
        ));
    }

    #[test]
    fn a_burst_is_allowed_then_refused_with_the_wait_until_the_next_request() {
        let clock = ManualClock::new(start());
        let limits = RateLimits::new(&tight(), Arc::new(clock.clone()));

        assert_eq!(limits.check_ip(ip("192.0.2.10")), Ok(()));
        assert_eq!(limits.check_ip(ip("192.0.2.10")), Ok(()));
        assert_eq!(
            limits.check_ip(ip("192.0.2.10")),
            Err(Refused {
                retry_after: Duration::from_millis(500)
            })
        );
        clock.advance(Duration::from_millis(500));
        assert_eq!(limits.check_ip(ip("192.0.2.10")), Ok(()));
    }

    #[test]
    fn each_limit_has_its_own_buckets() {
        let limits = RateLimits::new(&tight(), Arc::new(ManualClock::new(start())));
        for _ in 0..2 {
            limits.check_ip(ip("192.0.2.10")).unwrap();
        }

        assert!(limits.check_ip(ip("192.0.2.10")).is_err());
        assert_eq!(limits.check_ip(ip("198.51.100.10")), Ok(()));
        for class in [RateClass::Login, RateClass::Refresh, RateClass::Register] {
            assert_eq!(
                limits.check_class(class, ip("192.0.2.10")),
                Ok(()),
                "{class:?}"
            );
        }
        assert_eq!(limits.check_user(user(1)), Ok(()));
        assert_eq!(limits.check_grant_by_name(user(1)), Ok(()));
        assert_eq!(limits.check_enroll(ip("192.0.2.10")), Ok(()));
    }

    #[test]
    fn grants_by_name_are_refused_past_their_burst_apart_from_other_requests() {
        let clock = ManualClock::new(start());
        let limits = RateLimits::new(&tight(), Arc::new(clock.clone()));
        for _ in 0..2 {
            limits.check_grant_by_name(user(1)).unwrap();
        }

        assert_eq!(
            limits.check_grant_by_name(user(1)),
            Err(Refused {
                retry_after: Duration::from_millis(500)
            })
        );
        assert_eq!(limits.check_grant_by_name(user(2)), Ok(()));
        assert_eq!(limits.check_user(user(1)), Ok(()));
        clock.advance(Duration::from_millis(500));
        assert_eq!(limits.check_grant_by_name(user(1)), Ok(()));
    }

    #[test]
    fn enrollments_are_refused_past_their_burst_per_client_apart_from_other_requests() {
        let clock = ManualClock::new(start());
        let limits = RateLimits::new(&tight(), Arc::new(clock.clone()));
        limits.check_enroll(ip("2001:db8:0:1::1")).unwrap();
        limits.check_enroll(ip("2001:db8:0:1::2")).unwrap();

        assert_eq!(
            limits.check_enroll(ip("2001:db8:0:1:ffff::9")),
            Err(Refused {
                retry_after: Duration::from_millis(500)
            })
        );
        assert_eq!(limits.check_enroll(ip("2001:db8:0:2::1")), Ok(()));
        assert_eq!(limits.check_ip(ip("2001:db8:0:1::1")), Ok(()));
        clock.advance(Duration::from_millis(500));
        assert_eq!(limits.check_enroll(ip("2001:db8:0:1::1")), Ok(()));
    }

    fn fingerprint(n: u8) -> CertFingerprint {
        CertFingerprint::from_digest([n; 32])
    }

    #[test]
    fn connects_are_refused_past_their_burst_per_certificate_apart_from_other_requests() {
        let clock = ManualClock::new(start());
        let limits = RateLimits::new(&tight(), Arc::new(clock.clone()));
        limits.check_connect(fingerprint(1)).unwrap();
        limits.check_connect(fingerprint(1)).unwrap();

        assert_eq!(
            limits.check_connect(fingerprint(1)),
            Err(Refused {
                retry_after: Duration::from_millis(500)
            })
        );
        assert_eq!(limits.check_connect(fingerprint(2)), Ok(()));
        assert_eq!(limits.check_enroll(ip("192.0.2.10")), Ok(()));
        clock.advance(Duration::from_millis(500));
        assert_eq!(limits.check_connect(fingerprint(1)), Ok(()));
    }

    fn bot(n: u8) -> BotId {
        BotId::new_v7(start(), [n; 10]).unwrap()
    }

    #[test]
    fn bot_actions_are_refused_past_their_burst_per_bot_apart_from_other_requests() {
        let clock = ManualClock::new(start());
        let limits = RateLimits::new(&tight(), Arc::new(clock.clone()));
        limits.check_bot_action(bot(1)).unwrap();
        limits.check_bot_action(bot(1)).unwrap();

        assert_eq!(
            limits.check_bot_action(bot(1)),
            Err(Refused {
                retry_after: Duration::from_millis(500)
            })
        );
        assert_eq!(limits.check_bot_action(bot(2)), Ok(()));
        assert_eq!(limits.check_user(user(1)), Ok(()));
        clock.advance(Duration::from_millis(500));
        assert_eq!(limits.check_bot_action(bot(1)), Ok(()));
    }

    #[test]
    fn fresh_sessions_are_refused_past_their_burst_per_bot_apart_from_bot_actions() {
        let clock = ManualClock::new(start());
        let limits = RateLimits::new(&tight(), Arc::new(clock.clone()));
        limits.check_fresh_session(bot(1)).unwrap();
        limits.check_fresh_session(bot(1)).unwrap();

        assert_eq!(
            limits.check_fresh_session(bot(1)),
            Err(Refused {
                retry_after: Duration::from_millis(500)
            })
        );
        assert_eq!(limits.check_fresh_session(bot(2)), Ok(()));
        assert_eq!(limits.check_bot_action(bot(1)), Ok(()));
        clock.advance(Duration::from_millis(500));
        assert_eq!(limits.check_fresh_session(bot(1)), Ok(()));
    }

    #[test]
    fn the_standard_class_spends_nothing() {
        let limits = RateLimits::new(&tight(), Arc::new(ManualClock::new(start())));

        for _ in 0..10 {
            assert_eq!(
                limits.check_class(RateClass::Standard, ip("192.0.2.10")),
                Ok(())
            );
        }
        assert_eq!(limits.tracked(), 0);
    }

    #[test]
    fn a_class_and_a_user_are_refused_past_their_burst() {
        let limits = RateLimits::new(&tight(), Arc::new(ManualClock::new(start())));
        for class in [RateClass::Login, RateClass::Refresh, RateClass::Register] {
            for _ in 0..2 {
                limits.check_class(class, ip("192.0.2.10")).unwrap();
            }
            assert!(
                limits.check_class(class, ip("192.0.2.10")).is_err(),
                "{class:?}"
            );
        }
        for _ in 0..2 {
            limits.check_user(user(1)).unwrap();
        }

        assert!(limits.check_user(user(1)).is_err());
        assert_eq!(limits.check_user(user(2)), Ok(()));
    }

    #[test]
    fn an_ipv6_client_is_counted_by_its_64() {
        let limits = RateLimits::new(&tight(), Arc::new(ManualClock::new(start())));
        limits.check_ip(ip("2001:db8:0:1::1")).unwrap();
        limits.check_ip(ip("2001:db8:0:1::2")).unwrap();

        assert!(limits.check_ip(ip("2001:db8:0:1:ffff::9")).is_err());
        assert_eq!(limits.check_ip(ip("2001:db8:0:2::1")), Ok(()));
    }

    #[test]
    fn refusals_are_counted_per_limit_and_taken_once() {
        let limits = RateLimits::new(&tight(), Arc::new(ManualClock::new(start())));
        for _ in 0..5 {
            let _ = limits.check_ip(ip("192.0.2.10"));
            let _ = limits.check_class(RateClass::Login, ip("192.0.2.10"));
        }
        for _ in 0..3 {
            let _ = limits.check_user(user(1));
        }
        for _ in 0..3 {
            let _ = limits.check_enroll(ip("192.0.2.10"));
        }
        for _ in 0..4 {
            let _ = limits.check_grant_by_name(user(1));
        }
        for _ in 0..6 {
            let _ = limits.check_connect(fingerprint(1));
        }
        for _ in 0..7 {
            let _ = limits.check_bot_action(bot(1));
        }
        for _ in 0..8 {
            let _ = limits.check_fresh_session(bot(1));
        }
        let _ = limits.check_class(RateClass::Refresh, ip("192.0.2.10"));

        assert_eq!(
            limits.take_refusals(),
            Refusals {
                per_ip: 3,
                login: 3,
                refresh: 0,
                register: 0,
                per_user: 1,
                grants_by_name: 2,
                enroll: 1,
                connect: 4,
                bot_actions: 5,
                fresh_sessions: 6,
            }
        );
        assert_eq!(limits.take_refusals(), Refusals::default());
    }

    #[test]
    fn refusals_are_empty_only_when_every_count_is_zero() {
        assert!(Refusals::default().is_empty());
        for refusals in [
            Refusals {
                per_ip: 1,
                ..Refusals::default()
            },
            Refusals {
                login: 1,
                ..Refusals::default()
            },
            Refusals {
                refresh: 1,
                ..Refusals::default()
            },
            Refusals {
                register: 1,
                ..Refusals::default()
            },
            Refusals {
                per_user: 1,
                ..Refusals::default()
            },
            Refusals {
                grants_by_name: 1,
                ..Refusals::default()
            },
            Refusals {
                enroll: 1,
                ..Refusals::default()
            },
            Refusals {
                connect: 1,
                ..Refusals::default()
            },
            Refusals {
                bot_actions: 1,
                ..Refusals::default()
            },
            Refusals {
                fresh_sessions: 1,
                ..Refusals::default()
            },
        ] {
            assert!(!refusals.is_empty(), "{refusals:?}");
        }
    }

    #[test]
    fn retain_recent_forgets_a_client_one_interval_after_its_bucket_is_full_again() {
        let clock = ManualClock::new(start());
        let limits = RateLimits::new(&tight(), Arc::new(clock.clone()));
        limits.check_ip(ip("192.0.2.10")).unwrap();
        limits
            .check_class(RateClass::Login, ip("192.0.2.10"))
            .unwrap();
        limits.check_user(user(1)).unwrap();
        limits.check_grant_by_name(user(1)).unwrap();
        limits.check_enroll(ip("192.0.2.10")).unwrap();
        limits.check_connect(fingerprint(1)).unwrap();
        limits.check_bot_action(bot(1)).unwrap();
        limits.check_fresh_session(bot(1)).unwrap();
        clock.advance(Duration::from_millis(400));
        limits.check_ip(ip("198.51.100.10")).unwrap();
        assert_eq!(limits.tracked(), 9);
        // The first eight are full again at 500 ms, the later one at 900 ms;
        // each is dropped one 500 ms interval after that.
        clock.advance(Duration::from_millis(599));
        limits.retain_recent();
        assert_eq!(limits.tracked(), 9, "a millisecond short");

        clock.advance(Duration::from_millis(1));
        limits.retain_recent();

        assert_eq!(limits.tracked(), 1, "only the later client is kept");
        clock.advance(Duration::from_millis(400));
        limits.retain_recent();
        assert_eq!(limits.tracked(), 0);
    }

    #[test]
    fn the_debug_form_names_the_clock_and_the_count() {
        let limits = RateLimits::new(&tight(), Arc::new(ManualClock::new(start())));

        let shown = format!("{limits:?}");

        assert!(
            shown.starts_with("RateLimits { clock: LimiterClock"),
            "{shown}"
        );
        assert!(shown.contains("tracked: 0"), "{shown}");
    }
}
