//! `[auth]`: how the server authenticates users (Plan.md Appendix A,
//! ADR-0016).
//!
//! Group A adds the argon2 keys, group B the token lifetimes, group C the
//! lockout threshold, group E the invite lifetime.

use fleet_core::auth::{InviteTtl, LockoutPolicy, SessionPolicy};

/// The inclusive range of a numeric key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    /// The lowest valid value.
    pub min: u32,
    /// The highest valid value.
    pub max: u32,
}

impl Bounds {
    /// Whether `value` is within the range.
    #[must_use]
    pub const fn contains(self, value: u32) -> bool {
        value >= self.min && value <= self.max
    }
}

/// `[auth]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthConfig {
    /// How passwords are hashed (`argon2_*`).
    pub argon2: Argon2Params,
    /// How long sessions and their tokens live (`access_token_ttl_secs`,
    /// `refresh_token_idle_days`, `refresh_token_absolute_days`).
    pub sessions: SessionPolicy,
    /// How many failed logins lock a username (`lockout_threshold`).
    pub lockout: LockoutPolicy,
    /// How long an invite lives (`invite_ttl_hours`).
    pub invites: InviteTtl,
}

impl AuthConfig {
    /// `access_token_ttl_secs`: 1 minute to 1 hour. Default 900 (§7.2).
    pub const ACCESS_TOKEN_TTL_SECS: Bounds = Bounds { min: 60, max: 3600 };
    /// `refresh_token_idle_days`: 1 to 90 days, at most the absolute
    /// lifetime. Default 7.
    pub const REFRESH_TOKEN_IDLE_DAYS: Bounds = Bounds { min: 1, max: 90 };
    /// `refresh_token_absolute_days`: 1 to 365 days. Default 30.
    pub const REFRESH_TOKEN_ABSOLUTE_DAYS: Bounds = Bounds { min: 1, max: 365 };
    /// `lockout_threshold`: 3 to 20 failures. Default 5 (§7.4).
    pub const LOCKOUT_THRESHOLD: Bounds = Bounds {
        min: LockoutPolicy::MIN_THRESHOLD,
        max: LockoutPolicy::MAX_THRESHOLD,
    };
    /// `invite_ttl_hours`: 1 hour to 30 days. Default 72 (§7.2).
    pub const INVITE_TTL_HOURS: Bounds = Bounds {
        min: InviteTtl::MIN_HOURS,
        max: InviteTtl::MAX_HOURS,
    };
}

/// The password service's argon2 parameters (`[auth] argon2_*`), never below
/// §7.2's OWASP baseline (m = 19 MiB, t = 2, p = 1).
///
/// Config validation makes them; [`Argon2Params::try_new`] checks the same
/// ranges for anyone else, and `Default` is Appendix A's baseline. No way to
/// build them below the floor exists, not even for tests (ADR-0016): every
/// hash the server makes, in production and in tests, costs at least the
/// baseline. The upper bounds catch typos, and the password service also
/// refuses to verify a stored hash whose parameters exceed them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argon2Params {
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
    max_concurrent: u32,
}

/// Why argon2 parameters were refused: one of them is out of its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("`{key}` must be between {min} and {max}")]
pub struct Argon2ParamsError {
    /// The key, without its section (`argon2_memory_kib`).
    pub key: &'static str,
    /// The lowest valid value.
    pub min: u32,
    /// The highest valid value.
    pub max: u32,
}

impl Argon2Params {
    /// `argon2_memory_kib`: the memory per hash in KiB, 19 MiB to 1 GiB.
    pub const MEMORY_KIB: Bounds = Bounds {
        min: 19_456,
        max: 1_048_576,
    };
    /// `argon2_iterations`: the passes over the memory.
    pub const ITERATIONS: Bounds = Bounds { min: 2, max: 10 };
    /// `argon2_parallelism`: the lanes. Without argon2's `parallel` feature
    /// they're computed one after another, so more lanes cost time, not
    /// cores.
    pub const PARALLELISM: Bounds = Bounds { min: 1, max: 16 };
    /// `argon2_max_concurrent`: how many hashes may run at once.
    pub const MAX_CONCURRENT: Bounds = Bounds { min: 1, max: 64 };

    /// Checks each value against its range.
    ///
    /// # Errors
    /// [`Argon2ParamsError`] naming the first value out of its range.
    pub fn try_new(
        memory_kib: u32,
        iterations: u32,
        parallelism: u32,
        max_concurrent: u32,
    ) -> Result<Self, Argon2ParamsError> {
        for (key, value, bounds) in [
            ("argon2_memory_kib", memory_kib, Self::MEMORY_KIB),
            ("argon2_iterations", iterations, Self::ITERATIONS),
            ("argon2_parallelism", parallelism, Self::PARALLELISM),
            (
                "argon2_max_concurrent",
                max_concurrent,
                Self::MAX_CONCURRENT,
            ),
        ] {
            if !bounds.contains(value) {
                return Err(Argon2ParamsError {
                    key,
                    min: bounds.min,
                    max: bounds.max,
                });
            }
        }
        Ok(Self {
            memory_kib,
            iterations,
            parallelism,
            max_concurrent,
        })
    }

    /// The memory per hash, in KiB (argon2's `m`).
    #[must_use]
    pub const fn memory_kib(&self) -> u32 {
        self.memory_kib
    }

    /// The passes over the memory (argon2's `t`).
    #[must_use]
    pub const fn iterations(&self) -> u32 {
        self.iterations
    }

    /// The lanes (argon2's `p`).
    #[must_use]
    pub const fn parallelism(&self) -> u32 {
        self.parallelism
    }

    /// How many hashes may run at once.
    #[must_use]
    pub const fn max_concurrent(&self) -> u32 {
        self.max_concurrent
    }
}

impl Default for Argon2Params {
    /// Appendix A's values, §7.2's OWASP baseline: 19456 KiB, 2 iterations,
    /// 1 lane, 4 hashes at once.
    fn default() -> Self {
        Self {
            memory_kib: 19_456,
            iterations: 2,
            parallelism: 1,
            max_concurrent: 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn the_default_is_the_baseline_and_within_every_range() {
        let default = Argon2Params::default();

        assert_eq!(
            Argon2Params::try_new(19_456, 2, 1, 4),
            Ok(default),
            "the default is Appendix A's baseline"
        );
    }

    #[rstest]
    #[case::memory_below(19_455, 2, 1, 4, "argon2_memory_kib")]
    #[case::memory_above(1_048_577, 2, 1, 4, "argon2_memory_kib")]
    #[case::iterations_below(19_456, 1, 1, 4, "argon2_iterations")]
    #[case::iterations_above(19_456, 11, 1, 4, "argon2_iterations")]
    #[case::parallelism_below(19_456, 2, 0, 4, "argon2_parallelism")]
    #[case::parallelism_above(19_456, 2, 17, 4, "argon2_parallelism")]
    #[case::concurrency_below(19_456, 2, 1, 0, "argon2_max_concurrent")]
    #[case::concurrency_above(19_456, 2, 1, 65, "argon2_max_concurrent")]
    fn a_value_one_step_outside_its_range_is_refused(
        #[case] memory: u32,
        #[case] iterations: u32,
        #[case] parallelism: u32,
        #[case] concurrent: u32,
        #[case] key: &str,
    ) {
        let error = Argon2Params::try_new(memory, iterations, parallelism, concurrent).unwrap_err();

        assert_eq!(error.key, key);
    }

    #[test]
    fn the_extremes_are_accepted() {
        assert!(Argon2Params::try_new(1_048_576, 10, 16, 64).is_ok());
        assert!(Argon2Params::try_new(19_456, 2, 1, 1).is_ok());
    }

    #[test]
    fn the_error_names_the_key_and_its_range() {
        let error = Argon2Params::try_new(1, 2, 1, 4).unwrap_err();

        assert_eq!(
            error.to_string(),
            "`argon2_memory_kib` must be between 19456 and 1048576"
        );
    }
}
