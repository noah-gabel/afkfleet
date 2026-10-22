//! Validation: from [`RawConfig`] to [`ServerConfig`], reporting every
//! problem at once.

use core::num::NonZeroU32;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::TimeDelta;

use super::raw::{
    RawAccounts, RawAuth, RawChat, RawConfig, RawGrpc, RawLog, RawRateLimits, RawVault,
};
use super::{
    AccountsConfig, Argon2Params, AuthConfig, Bounds, ChatConfig, ConfigError, DatabaseConfig,
    GrpcConfig, Heartbeats, HttpConfig, KeyPath, LinkFlowLimits, LogConfig, LogFilter, LogFormat,
    PkiPaths, ProblemKind, Problems, Rate, RateLimitConfig, ServerConfig, VaultConfig,
};
use fleet_core::auth::{InviteTtl, LockoutPolicy, SessionPolicy};
use fleet_core::authz::{CommandAllowlist, CommandAllowlistError};
use fleet_core::chat::ChatRate;

use crate::http::rate_limit::{IpNet, TrustedProxies};

/// Validates `raw`: garde's ranges first (`[http]`'s and `[rate_limits]`'s),
/// then each text key, `[auth]`'s ranges and the trusted proxies.
///
/// # Errors
/// [`ConfigError::Invalid`](fleet_startup::config::ConfigError::Invalid)
/// with every problem, sorted by key.
pub(crate) fn validate(raw: &RawConfig) -> Result<ServerConfig, ConfigError> {
    let mut problems = Problems::default();
    if let Err(report) = garde::Validate::validate(raw) {
        for (path, error) in report.iter() {
            let key = path
                .to_string()
                .split('.')
                .fold(KeyPath::root(), |key, part| key.key(part));
            problems.push(
                key,
                ProblemKind::OutOfRange {
                    message: error.message().to_owned(),
                },
            );
        }
    }

    let bind = bind(raw.http.bind.as_deref(), &mut problems);
    let path = database_path(raw.database.path.as_deref(), &mut problems);
    let log = log_config(&raw.log, &mut problems);
    let argon2 = argon2(&raw.auth, &mut problems);
    let sessions = sessions(&raw.auth, &mut problems);
    let vault = vault(&raw.vault, &mut problems);
    let trusted_proxies = trusted_proxies(&raw.http.trusted_proxies, &mut problems);
    let lockout = lockout(&raw.auth, &mut problems);
    let invites = invites(&raw.auth, &mut problems);
    let rate_limits = rate_limits(&raw.rate_limits);
    let accounts = accounts(&raw.accounts);
    let chat = chat(&raw.chat, &mut problems);
    let grpc = grpc(raw.grpc.as_ref(), bind, &mut problems);

    let (
        true,
        Some(bind),
        Some(trusted_proxies),
        Some(path),
        Some(log),
        Some(argon2),
        Some(sessions),
        Some(lockout),
        Some(invites),
        Some(vault),
        Some(rate_limits),
        Some(accounts),
        Some(chat),
        Ok(grpc),
    ) = (
        problems.is_empty(),
        bind,
        trusted_proxies,
        path,
        log,
        argon2,
        sessions,
        lockout,
        invites,
        vault,
        rate_limits,
        accounts,
        chat,
        grpc,
    )
    else {
        return Err(ConfigError::Invalid(problems.sorted()));
    };
    Ok(ServerConfig {
        dev_mode: raw.dev_mode,
        http: HttpConfig {
            bind,
            request_timeout: Duration::from_secs(raw.http.request_timeout_secs),
            // garde has checked it's at most 1 MiB, which fits any usize
            // afkfleet builds for.
            max_body_bytes: usize::try_from(raw.http.max_body_bytes).unwrap_or(usize::MAX),
            trusted_proxies,
        },
        database: DatabaseConfig { path },
        log,
        auth: AuthConfig {
            argon2,
            sessions,
            lockout,
            invites,
        },
        vault,
        rate_limits,
        accounts,
        chat,
        grpc,
    })
}

/// A section with problems, each already reported.
struct Invalid;

/// `[grpc]`: absent is `Ok(None)`, the control plane off. Present, its four
/// files are required, absolute and different from each other; `bind` is
/// an IP address with a port other than 0 and other than `http_bind`'s.
/// [`Invalid`] when anything is wrong.
fn grpc(
    raw: Option<&RawGrpc>,
    http_bind: Option<SocketAddr>,
    problems: &mut Problems,
) -> Result<Option<GrpcConfig>, Invalid> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let section = KeyPath::root().key("grpc");
    let bind = grpc_bind(raw.bind.as_deref(), http_bind, &section, problems);
    let heartbeats = grpc_heartbeats(raw, problems);
    // Each file in the order the section lists them, so a repeated path is
    // reported at the later key, naming the earlier one.
    let files = [
        ("ca_cert_file", raw.ca_cert_file.as_deref()),
        ("ca_key_file", raw.ca_key_file.as_deref()),
        ("server_cert_file", raw.server_cert_file.as_deref()),
        ("server_key_file", raw.server_key_file.as_deref()),
    ];
    let mut paths: Vec<(&'static str, PathBuf)> = Vec::with_capacity(files.len());
    let mut valid = true;
    for (name, text) in files {
        let key = section.key(name);
        let Some(text) = text else {
            problems.push(key, ProblemKind::Missing);
            valid = false;
            continue;
        };
        match absolute_path(text) {
            Ok(path) => {
                if let Some((earlier, _)) = paths.iter().find(|(_, seen)| *seen == path) {
                    problems.push(key, ProblemKind::SameFileAs { key: earlier });
                    valid = false;
                }
                paths.push((name, path));
            }
            Err(kind) => {
                problems.push(key, kind);
                valid = false;
            }
        }
    }
    // garde has checked the lifetime's range, so only an error that can't
    // happen remains.
    let lifetime = TimeDelta::try_days(i64::from(raw.agent_cert_lifetime_days));
    let (
        [ca_cert, ca_key, server_cert, server_key],
        true,
        Some(bind),
        Some(lifetime),
        Some(heartbeats),
    ) = (paths.as_slice(), valid, bind, lifetime, heartbeats)
    else {
        return Err(Invalid);
    };
    Ok(Some(GrpcConfig {
        bind,
        pki: PkiPaths {
            ca_cert_file: ca_cert.1.clone(),
            ca_key_file: ca_key.1.clone(),
            server_cert_file: server_cert.1.clone(),
            server_key_file: server_key.1.clone(),
        },
        agent_cert_lifetime: lifetime,
        heartbeats,
    }))
}

/// `[grpc]`'s heartbeat keys: each against its range, then the interval
/// against half the timeout, which is checked only when both are in range,
/// so one mistake isn't reported twice (ADR-0019, S23).
fn grpc_heartbeats(raw: &RawGrpc, problems: &mut Problems) -> Option<Heartbeats> {
    let in_range = check_ranges(
        "grpc",
        [
            (
                "heartbeat_interval_secs",
                raw.heartbeat_interval_secs,
                Heartbeats::INTERVAL_SECS,
            ),
            (
                "heartbeat_timeout_secs",
                raw.heartbeat_timeout_secs,
                Heartbeats::TIMEOUT_SECS,
            ),
        ],
        problems,
    );
    if !in_range {
        return None;
    }
    let heartbeats =
        Heartbeats::try_from_secs(raw.heartbeat_interval_secs, raw.heartbeat_timeout_secs);
    if heartbeats.is_err() {
        problems.push(
            KeyPath::root().key("grpc").key("heartbeat_interval_secs"),
            ProblemKind::MoreThanHalfOfKey {
                key: "heartbeat_timeout_secs",
            },
        );
    }
    heartbeats.ok()
}

/// `[grpc] bind`: an IP address and a port other than 0 and other than
/// `[http] bind`'s; missing means [`GrpcConfig::DEFAULT_BIND`].
fn grpc_bind(
    raw: Option<&str>,
    http_bind: Option<SocketAddr>,
    section: &KeyPath,
    problems: &mut Problems,
) -> Option<SocketAddr> {
    let key = section.key("bind");
    let address = match raw {
        None => GrpcConfig::DEFAULT_BIND,
        Some(text) => {
            let Ok(address) = text.parse::<SocketAddr>() else {
                problems.push(key, ProblemKind::SocketAddress);
                return None;
            };
            address
        }
    };
    if address.port() == 0 {
        problems.push(key, ProblemKind::PortZero);
        return None;
    }
    if http_bind.is_some_and(|http| http.port() == address.port()) {
        problems.push(key, ProblemKind::SamePortAs { key: "http.bind" });
        return None;
    }
    Some(address)
}

/// `[vault]`: both keys required; 1 to 16 key files, each under a key ID
/// from 1 to 4294967295 written without leading zeros, with an absolute
/// path; the active key ID among them. Whether the active key is listed is
/// checked only when the list itself is valid, so one mistake isn't
/// reported twice.
fn vault(raw: &RawVault, problems: &mut Problems) -> Option<VaultConfig> {
    let section = KeyPath::root().key("vault");
    let files_key = section.key("master_key_files");
    let active_key = section.key("active_key_id");
    let key_files = match &raw.master_key_files {
        None => {
            problems.push(files_key, ProblemKind::Missing);
            None
        }
        Some(entries) => key_files(entries, &files_key, problems),
    };
    let active = match raw.active_key_id {
        None => {
            problems.push(active_key.clone(), ProblemKind::Missing);
            None
        }
        Some(id) => {
            let id = u32::try_from(id).ok().filter(|id| *id >= 1);
            if id.is_none() {
                problems.push(active_key.clone(), ProblemKind::KeyId);
            }
            id
        }
    };
    let (Some(key_files), Some(active_key_id)) = (key_files, active) else {
        return None;
    };
    if !key_files.contains_key(&active_key_id) {
        problems.push(active_key, ProblemKind::UnknownKeyId);
        return None;
    }
    Some(VaultConfig {
        key_files,
        active_key_id,
    })
}

/// `[vault] master_key_files`: every entry is checked, so every problem is
/// reported at once.
fn key_files(
    entries: &BTreeMap<String, String>,
    key: &KeyPath,
    problems: &mut Problems,
) -> Option<BTreeMap<u32, PathBuf>> {
    let mut valid = true;
    if entries.is_empty() || entries.len() > VaultConfig::MAX_KEY_FILES {
        problems.push(
            key.clone(),
            ProblemKind::KeyCount {
                max: VaultConfig::MAX_KEY_FILES,
            },
        );
        valid = false;
    }
    let mut files = BTreeMap::new();
    for (id, path) in entries {
        let entry = key.key(id);
        let Some(id) = key_id(id) else {
            problems.push(entry, ProblemKind::KeyId);
            valid = false;
            continue;
        };
        match absolute_path(path) {
            Ok(path) => {
                files.insert(id, path);
            }
            Err(kind) => {
                problems.push(entry, kind);
                valid = false;
            }
        }
    }
    valid.then_some(files)
}

/// A key ID as written in the config: a whole number from 1 to 4294967295,
/// in its one canonical form (no sign, no leading zeros), so two entries
/// can't name the same key.
fn key_id(text: &str) -> Option<u32> {
    let id: u32 = text.parse().ok()?;
    (id >= 1 && id.to_string() == text).then_some(id)
}

/// Checks each `(key, value, bounds)` of `section` against its range and
/// reports every problem under its key, in garde's words ("lower than",
/// "greater than"), like the other numeric keys. Whether all were in range.
fn check_ranges<const N: usize>(
    section: &'static str,
    checks: [(&'static str, u32, Bounds); N],
    problems: &mut Problems,
) -> bool {
    let section = KeyPath::root().key(section);
    let mut valid = true;
    for (key, value, bounds) in checks {
        let message = if value < bounds.min {
            format!("lower than {}", bounds.min)
        } else if value > bounds.max {
            format!("greater than {}", bounds.max)
        } else {
            continue;
        };
        problems.push(section.key(key), ProblemKind::OutOfRange { message });
        valid = false;
    }
    valid
}

/// `[auth]`'s token lifetimes: each against its range, then the idle
/// lifetime against the absolute one, which is checked only when both are
/// in range, so one mistake isn't reported twice.
fn sessions(raw: &RawAuth, problems: &mut Problems) -> Option<SessionPolicy> {
    let in_range = check_ranges(
        "auth",
        [
            (
                "access_token_ttl_secs",
                raw.access_token_ttl_secs,
                AuthConfig::ACCESS_TOKEN_TTL_SECS,
            ),
            (
                "refresh_token_idle_days",
                raw.refresh_token_idle_days,
                AuthConfig::REFRESH_TOKEN_IDLE_DAYS,
            ),
            (
                "refresh_token_absolute_days",
                raw.refresh_token_absolute_days,
                AuthConfig::REFRESH_TOKEN_ABSOLUTE_DAYS,
            ),
        ],
        problems,
    );
    if !in_range {
        return None;
    }
    if raw.refresh_token_idle_days > raw.refresh_token_absolute_days {
        problems.push(
            KeyPath::root().key("auth").key("refresh_token_idle_days"),
            ProblemKind::GreaterThanKey {
                key: "refresh_token_absolute_days",
            },
        );
        return None;
    }
    // In range, an hour is always shorter than a day, so only an error that
    // can't happen remains.
    SessionPolicy::try_new(
        Duration::from_secs(u64::from(raw.access_token_ttl_secs)),
        days(raw.refresh_token_idle_days),
        days(raw.refresh_token_absolute_days),
    )
    .ok()
}

/// `n` days.
fn days(n: u32) -> Duration {
    Duration::from_hours(u64::from(n) * 24)
}

/// `[auth] argon2_*`: each value against its range in `Argon2Params`, with
/// every problem reported under its key, in garde's words ("lower than",
/// "greater than"), like the other numeric keys.
fn argon2(raw: &RawAuth, problems: &mut Problems) -> Option<Argon2Params> {
    check_ranges(
        "auth",
        [
            (
                "argon2_memory_kib",
                raw.memory_kib,
                Argon2Params::MEMORY_KIB,
            ),
            (
                "argon2_iterations",
                raw.iterations,
                Argon2Params::ITERATIONS,
            ),
            (
                "argon2_parallelism",
                raw.parallelism,
                Argon2Params::PARALLELISM,
            ),
            (
                "argon2_max_concurrent",
                raw.max_concurrent,
                Argon2Params::MAX_CONCURRENT,
            ),
        ],
        problems,
    );
    Argon2Params::try_new(
        raw.memory_kib,
        raw.iterations,
        raw.parallelism,
        raw.max_concurrent,
    )
    .ok()
}

/// `[http] trusted_proxies`: at most [`TrustedProxies::MAX`] networks in
/// strict CIDR form, none listed twice. Every problem is reported at once,
/// each entry's under its index; none echoes the text.
fn trusted_proxies(raw: &[String], problems: &mut Problems) -> Option<TrustedProxies> {
    let key = KeyPath::root().key("http").key("trusted_proxies");
    let mut valid = true;
    if raw.len() > TrustedProxies::MAX {
        problems.push(
            key.clone(),
            ProblemKind::TooManyEntries {
                max: TrustedProxies::MAX,
            },
        );
        valid = false;
    }
    let mut networks = Vec::with_capacity(raw.len());
    for (index, text) in raw.iter().enumerate() {
        match text.parse::<IpNet>() {
            Ok(network) if networks.contains(&network) => {
                problems.push(key.index(index), ProblemKind::Duplicate);
                valid = false;
            }
            Ok(network) => networks.push(network),
            Err(error) => {
                problems.push(key.index(index), ProblemKind::IpNet(error));
                valid = false;
            }
        }
    }
    valid.then(|| TrustedProxies::new(networks))
}

/// `[auth] lockout_threshold`, in garde's words like the other `[auth]`
/// keys.
fn lockout(raw: &RawAuth, problems: &mut Problems) -> Option<LockoutPolicy> {
    check_ranges(
        "auth",
        [(
            "lockout_threshold",
            raw.lockout_threshold,
            AuthConfig::LOCKOUT_THRESHOLD,
        )],
        problems,
    );
    LockoutPolicy::try_new(raw.lockout_threshold).ok()
}

/// `[auth] invite_ttl_hours`, in garde's words like the other `[auth]`
/// keys.
fn invites(raw: &RawAuth, problems: &mut Problems) -> Option<InviteTtl> {
    check_ranges(
        "auth",
        [(
            "invite_ttl_hours",
            raw.invite_ttl_hours,
            AuthConfig::INVITE_TTL_HOURS,
        )],
        problems,
    );
    InviteTtl::try_new(raw.invite_ttl_hours).ok()
}

/// `[rate_limits]`, whose ranges garde has checked: none is 0, so only an
/// error that can't happen remains.
fn rate_limits(raw: &RawRateLimits) -> Option<RateLimitConfig> {
    let count = NonZeroU32::new;
    Some(RateLimitConfig {
        per_ip: Rate::per_second(count(raw.per_ip_per_sec)?, count(raw.per_ip_burst)?),
        login: Rate::per_minute(count(raw.login_per_ip_per_min)?),
        refresh: Rate::per_minute(count(raw.refresh_per_ip_per_min)?),
        register: Rate::per_hour(count(raw.register_per_ip_per_hour)?),
        per_user: Rate::per_second(count(raw.per_user_per_sec)?, count(raw.per_user_burst)?),
        link_flows: LinkFlowLimits {
            total: count(raw.link_flows_total)?,
            per_user_per_day: count(raw.link_flows_per_user_per_day)?,
        },
        grants_by_name: Rate::per_hour(count(raw.grants_by_name_per_user_per_hour)?),
        enroll: Rate::per_minute(count(raw.enroll_per_ip_per_min)?),
        connect: Rate::per_minute(count(raw.connect_per_cert_per_min)?),
        bot_actions: Rate::per_minute(count(raw.bot_actions_per_bot_per_min)?),
        fresh_sessions: Rate::per_hour(count(raw.fresh_sessions_per_bot_per_hour)?),
        chat_per_bot: ChatRate::try_new(raw.chat_per_bot_interval_secs, raw.chat_per_bot_burst)
            .ok()?,
        chat_per_user: Rate::per_minute(count(raw.chat_per_user_per_min)?),
        ws_connections_per_user: count(raw.ws_connections_per_user)?,
    })
}

/// `[chat] command_allowlist`: the first bad entry is reported under its
/// index, never with its text; too many under the key.
fn chat(raw: &RawChat, problems: &mut Problems) -> Option<ChatConfig> {
    let key = KeyPath::root().key("chat").key("command_allowlist");
    match CommandAllowlist::try_new(&raw.command_allowlist) {
        Ok(command_allowlist) => Some(ChatConfig { command_allowlist }),
        Err(error) => {
            let at = match error {
                CommandAllowlistError::TooMany { .. } => key,
                CommandAllowlistError::Invalid { index, .. }
                | CommandAllowlistError::MissingSlash { index }
                | CommandAllowlistError::EmptyName { index }
                | CommandAllowlistError::HasArguments { index } => key.index(index),
            };
            problems.push(at, ProblemKind::CommandAllowlist(error));
            None
        }
    }
}

/// `[accounts]`, whose ranges garde has checked: none is 0, so only an
/// error that can't happen remains.
fn accounts(raw: &RawAccounts) -> Option<AccountsConfig> {
    Some(AccountsConfig {
        max_per_member: NonZeroU32::new(raw.max_per_member)?,
        allow_offline: raw.allow_offline,
    })
}

/// `[http] bind`: an IP address and a port other than 0; missing means
/// [`HttpConfig::DEFAULT_BIND`].
fn bind(raw: Option<&str>, problems: &mut Problems) -> Option<SocketAddr> {
    let key = KeyPath::root().key("http").key("bind");
    let Some(text) = raw else {
        return Some(HttpConfig::DEFAULT_BIND);
    };
    let Ok(address) = text.parse::<SocketAddr>() else {
        problems.push(key, ProblemKind::SocketAddress);
        return None;
    };
    if address.port() == 0 {
        problems.push(key, ProblemKind::PortZero);
        return None;
    }
    Some(address)
}

/// `[database] path`: required, not empty, and absolute.
fn database_path(raw: Option<&str>, problems: &mut Problems) -> Option<PathBuf> {
    let key = KeyPath::root().key("database").key("path");
    let Some(text) = raw else {
        problems.push(key, ProblemKind::Missing);
        return None;
    };
    absolute_path(text)
        .map_err(|kind| problems.push(key, kind))
        .ok()
}

/// A path that must be absolute: not empty, not relative.
fn absolute_path(text: &str) -> Result<PathBuf, ProblemKind> {
    if text.is_empty() {
        return Err(ProblemKind::Empty);
    }
    let path = Path::new(text);
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Err(ProblemKind::NotAbsolute)
    }
}

/// `[log]`: a missing key takes its default.
fn log_config(raw: &RawLog, problems: &mut Problems) -> Option<LogConfig> {
    let section = KeyPath::root().key("log");
    let format = match raw.format.as_deref() {
        None => Some(LogFormat::default()),
        Some(name) => name
            .parse()
            .map_err(|error| problems.push(section.key("format"), ProblemKind::LogFormat(error)))
            .ok(),
    };
    let filter = match raw.filter.as_deref() {
        None => Some(LogFilter::default()),
        Some(text) => LogFilter::try_from(text)
            .map_err(|error| problems.push(section.key("filter"), ProblemKind::LogFilter(error)))
            .ok(),
    };
    Some(LogConfig {
        format: format?,
        filter: filter?,
    })
}
