//! Why the server's config is invalid: its kinds of problems, and its names
//! for fleet-startup's generic error types (ADR-0015).

use fleet_core::authz::CommandAllowlistError;
use fleet_startup::config::{LogFilterError, UnknownLogFormatError};

use fleet_core::net::IpNetError;

/// Why [`load`](super::load) failed.
pub type ConfigError = fleet_startup::config::ConfigError<ProblemKind>;

/// Every problem the server's validation found, sorted by key.
pub type Problems = fleet_startup::config::Problems<ProblemKind>;

/// One problem, at one key.
pub type Problem = fleet_startup::config::Problem<ProblemKind>;

/// What's wrong with a key. No message echoes the value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProblemKind {
    /// A required key is missing.
    #[error("missing")]
    Missing,
    /// A required text is empty.
    #[error("must not be empty")]
    Empty,
    /// A number outside its range, with garde's message (`lower than 1`).
    #[error("{message}")]
    OutOfRange {
        /// garde's message, which names the bound, never the value.
        message: String,
    },
    /// A path that must be absolute isn't.
    #[error("must be an absolute path")]
    NotAbsolute,
    /// Not an IP address with a port.
    #[error("must be an IP address and a port, like 127.0.0.1:8080")]
    SocketAddress,
    /// Port 0, which would make the OS pick a port.
    #[error("port 0 isn't allowed: the server needs a fixed port")]
    PortZero,
    /// An unknown log format.
    #[error(transparent)]
    LogFormat(UnknownLogFormatError),
    /// An invalid log filter.
    #[error(transparent)]
    LogFilter(LogFilterError),
    /// Not a key ID: a whole number from 1 to 4294967295, without leading
    /// zeros.
    #[error("must be a key ID from 1 to 4294967295, without leading zeros")]
    KeyId,
    /// Too few or too many key files.
    #[error("must list 1 to {max} key files")]
    KeyCount {
        /// The most allowed.
        max: usize,
    },
    /// The active key ID has no key file.
    #[error("must be one of the key IDs in `master_key_files`")]
    UnknownKeyId,
    /// A number greater than another key of its section allows, like an
    /// idle lifetime longer than the absolute one.
    #[error("greater than `{key}`")]
    GreaterThanKey {
        /// The other key, without its section.
        key: &'static str,
    },
    /// A number greater than half of another key of its section, like a
    /// heartbeat interval over half the heartbeat timeout.
    #[error("more than half of `{key}`")]
    MoreThanHalfOfKey {
        /// The other key, without its section.
        key: &'static str,
    },
    /// Not an IP network in strict CIDR form (`[http] trusted_proxies`).
    #[error("{}", proxy_network(*.0))]
    IpNet(IpNetError),
    /// A list with more entries than allowed.
    #[error("must list at most {max} entries")]
    TooManyEntries {
        /// The most allowed.
        max: usize,
    },
    /// An entry that an earlier one of the same list already names.
    #[error("is listed twice")]
    Duplicate,
    /// A path that another key of its section already names
    /// (`[grpc]`'s files).
    #[error("is the same file as `{key}`")]
    SameFileAs {
        /// The other key, without its section.
        key: &'static str,
    },
    /// A command allowlist entry that isn't `/` and a command name, or too
    /// many of them (`[chat] command_allowlist`); the entry's text is never
    /// shown.
    #[error(transparent)]
    CommandAllowlist(CommandAllowlistError),
    /// A listener's port that another listener already uses
    /// (`[grpc] bind` and `[http] bind`).
    #[error("uses the same port as `{key}`")]
    SamePortAs {
        /// The other key, with its section.
        key: &'static str,
    },
}

/// `error` as `[http] trusted_proxies` words it: a /0 would trust every
/// address, past every per-IP limit (ADR-0016).
fn proxy_network(error: IpNetError) -> String {
    match error {
        IpNetError::Everything => {
            "a prefix length of 0 would trust every address; list only your proxies' own networks"
                .to_owned()
        }
        IpNetError::Shape
        | IpNetError::PrefixLength { .. }
        | IpNetError::HostBits
        | IpNetError::MappedIpv4 => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use fleet_startup::config::KeyPath;

    use super::*;

    #[test]
    fn problems_are_listed_one_per_line_and_sorted() {
        let mut problems = Problems::default();
        problems.push(
            KeyPath::root().key("http").key("bind"),
            ProblemKind::PortZero,
        );
        problems.push(
            KeyPath::root().key("database").key("path"),
            ProblemKind::NotAbsolute,
        );
        let problems = problems.sorted();

        assert_eq!(
            ConfigError::Invalid(problems).to_string(),
            "the config is invalid:\n  \
             - database.path: must be an absolute path\n  \
             - http.bind: port 0 isn't allowed: the server needs a fixed port"
        );
    }

    #[test]
    fn a_zero_prefix_proxy_says_it_would_trust_every_address() {
        let everything = ProblemKind::IpNet(IpNetError::Everything).to_string();

        assert_eq!(
            everything,
            "a prefix length of 0 would trust every address; list only your proxies' own networks"
        );
        for other in [
            IpNetError::Shape,
            IpNetError::HostBits,
            IpNetError::MappedIpv4,
        ] {
            assert_eq!(ProblemKind::IpNet(other).to_string(), other.to_string());
        }
    }
}
