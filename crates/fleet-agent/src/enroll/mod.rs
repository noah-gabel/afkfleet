//! `afkfleet-agent enroll` (Plan.md P10.3; ADR-0019, S15, S18, S19, S20):
//! the agent trades an enrollment token for its identity, a key it makes
//! itself and a certificate the server signs, and stores both owner-only.
//!
//! main.rs loads the config (it must have `[control_plane]`), reads the
//! bundle from stdin ([`bundle`]) before any runtime starts, sets up logging
//! on stderr (stdout carries the result) and the TLS provider, and runs
//! [`enroll`] with the OS's randomness. In this order, the first failure
//! stopping the rest:
//! 1. **The pin** ([`pin`]): `ca_cert_file` must be the CA the bundle
//!    names; otherwise nothing is sent, and both fingerprints are shown.
//! 2. **No identity yet** ([`files`]): nothing at `key_file` or
//!    `cert_file`, their folders there, and their `.tmp` locks taken.
//! 3. **The key** ([`key`]): made from the randomness and written into its
//!    temp file before the call; only its CSR is sent.
//! 4. **The call** ([`client`]): `Enroll` over TLS that trusts only the
//!    pinned CA.
//! 5. **The certificate** ([`certificate`]): one PEM certificate the pinned
//!    CA signed for client authentication, for the agent's key.
//! 6. **Publish:** both files linked to their names, never replacing one.
//!
//! **Exit codes** ([`ENROLL_EXIT_CODES`]): 0 enrolled, 1 nothing was
//! enrolled, 2 usage, and 3 when the server enrolled the agent and used the
//! token but this host didn't store it (steps 5 and 6), or may have: the
//! request may have reached the server and no clear answer came back (step
//! 4; [`client::Unclear`]). That agent must be disabled, and a new token
//! used.
//!
//! **Output:** on success, stdout says the agent's ID, its name, the
//! certificate's end and the two files; on Windows, stderr adds that only
//! the key's folder protects it (S20). A failure is one `afkfleet-agent: …`
//! line on stderr, a fixed text per case, never the server's message or the
//! token.

pub mod bundle;
pub mod certificate;
pub mod client;
pub mod files;
pub mod key;
pub mod pin;

use std::io::Write;
use std::path::Path;

use chrono::SecondsFormat;
use fleet_core::enrollment::EnrollmentBundle;
use fleet_core::id::AgentId;
use fleet_core::system::SecureRandom;
use fleet_core::value::AgentName;
use fleet_proto::enrollment::EnrollRequest;
use fleet_proto::generated as wire;
use rustls::pki_types::UnixTime;
use tokio::runtime::Runtime;
use tracing::{error, info};

use self::client::{CallError, Refusal, Unclear};
use self::files::Claimed;
use self::key::AgentKey;
use self::pin::pinned_ca;
use crate::config::ControlPlaneConfig;

/// The exit codes of `afkfleet-agent enroll`, shown in its help.
pub const ENROLL_EXIT_CODES: &str = "\
Exit codes:
  0  enrolled: the key and the certificate are written
  1  nothing was enrolled: the config, logging, stdin, the CA pin, an
     identity that exists already, the key, or the server refused or
     wasn't reached
  2  usage error
  3  the server enrolled the agent and used the token, or may have (no
     clear answer came back after the request was sent), but this host
     didn't store it: disable that agent, then enroll with a new token";

/// How `enroll` ended, which decides the process's exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The key and the certificate are written.
    Enrolled,
    /// Nothing was enrolled, and nothing is left on the host.
    Failed,
    /// The server enrolled the agent, but this host didn't store it.
    Lost,
}

impl Exit {
    /// The process's exit code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Enrolled => 0,
            Self::Failed => 1,
            Self::Lost => 3,
        }
    }
}

/// What `enroll` needs from the config and stdin.
#[derive(Debug)]
pub struct Enrollment<'a> {
    /// `[control_plane]`.
    pub plane: &'a ControlPlaneConfig,
    /// The agent's name.
    pub name: &'a AgentName,
    /// The bundle read from stdin.
    pub bundle: EnrollmentBundle,
}

/// Enrolls the agent: see the [module docs](self). The key comes from
/// `random`, the call runs on `runtime`, the result goes to `out` and every
/// refusal to `err`.
pub fn enroll(
    enrollment: Enrollment<'_>,
    random: &dyn SecureRandom,
    runtime: &Runtime,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Exit {
    let Enrollment {
        plane,
        name,
        bundle,
    } = enrollment;
    let ca = match pinned_ca(&plane.ca_cert_file, bundle.ca_fingerprint()) {
        Ok(ca) => ca,
        Err(error) => return refuse(err, &chain(&error)),
    };
    let claimed = match Claimed::claim(&plane.key_file, &plane.cert_file) {
        Ok(claimed) => claimed,
        Err(error) => return refuse(err, &chain(&error)),
    };
    let made = AgentKey::generate(random).and_then(|key| key.csr_der().map(|csr| (key, csr)));
    let (key, csr_der) = match made {
        Ok(made) => made,
        Err(error) => {
            return refuse(
                err,
                &format!("the agent's key can't be made: {}", chain(&error)),
            );
        }
    };
    if let Err(error) = claimed.write_key(key.to_pem().as_bytes()) {
        return refuse(err, &chain(&error));
    }
    let request = wire::EnrollRequest::from(EnrollRequest {
        token: bundle.token().clone(),
        name: name.clone(),
        csr_der,
    });
    drop(bundle);
    let answer = match runtime.block_on(client::call(&plane.url, &ca, request)) {
        Ok(answer) => answer,
        Err(CallError::Answer(_)) => {
            return lost(err, None, "the server's answer isn't valid");
        }
        Err(CallError::Unclear(unclear)) => return unclear_end(err, name, unclear),
        Err(error) => return refuse(err, &call_failure(&error, plane, name)),
    };
    // From here on, the server has enrolled the agent and used the token.
    let agent = answer.agent_id;
    if let Err(problem) =
        certificate::check(&answer.certificate_pem, &ca.der, &key, UnixTime::now())
    {
        return lost(err, Some(agent), &problem.to_string());
    }
    if let Err(error) = claimed.publish(answer.certificate_pem.as_bytes()) {
        return lost(err, Some(agent), &chain(&error));
    }
    info!(agent_id = %agent, not_after = %answer.not_after, "the agent enrolled");
    let _ = writeln!(
        out,
        "enrolled as agent {agent} ({name})\ncertificate valid until {}\nkey: {}\ncertificate: {}",
        answer.not_after.to_rfc3339_opts(SecondsFormat::Secs, true),
        plane.key_file.display(),
        plane.cert_file.display()
    );
    if cfg!(windows) {
        let _ = writeln!(err, "{}", windows_note(&plane.key_file));
    }
    Exit::Enrolled
}

/// What Windows users are told about the key: only its folder protects it
/// there (ADR-0019, S20).
#[must_use]
pub fn windows_note(key: &Path) -> String {
    format!(
        "afkfleet-agent: on Windows, only its folder's permissions protect {}: keep it in a folder only you can open, such as one under your user profile",
        key.display()
    )
}

/// The fixed text of a call that enrolled nothing: never the server's
/// message.
fn call_failure(error: &CallError, plane: &ControlPlaneConfig, name: &AgentName) -> String {
    match error {
        CallError::BadUrl => "[control_plane] url can't be dialed".to_owned(),
        CallError::Unreachable(_) => format!(
            "the server can't be reached at {}: check [control_plane] url, and that the server runs with [grpc]",
            plane.url
        ),
        CallError::Certificate(_) => format!(
            "the server at {} has no certificate the pinned CA signed for its name: check [control_plane] url, and the names of the server's `pki server-cert`",
            plane.url
        ),
        CallError::TimedOut => {
            "the server couldn't be reached within 30 s; nothing was sent".to_owned()
        }
        CallError::Refused(refusal) => refused(*refusal, name),
        // Both are exit 3, handled before.
        CallError::Answer(_) => "the server's answer isn't valid".to_owned(),
        CallError::Unclear(_) => "no clear answer came back".to_owned(),
    }
}

/// The fixed text of a refusal.
fn refused(refusal: Refusal, name: &AgentName) -> String {
    match refusal {
        Refusal::InvalidToken => "the enrollment token isn't valid: it's unknown, used or expired (a token lives 15 minutes); create a new one".to_owned(),
        Refusal::NameTaken => format!(
            "an enabled agent is named {name} already: give this agent another name in its config"
        ),
        Refusal::Invalid | Refusal::TooLarge => "the server refused the enrollment request; is this agent older than the server?".to_owned(),
        Refusal::RateLimited {
            retry_after: Some(seconds),
        } => format!("too many enrollments from this address; try again in {seconds} s"),
        Refusal::RateLimited { retry_after: None } => {
            "too many enrollments from this address; try again later".to_owned()
        }
        Refusal::Unavailable => "the server is busy; try again in a moment".to_owned(),
        Refusal::TooSlow => "the server took too long to enroll this agent; try again".to_owned(),
        Refusal::Other(code) => format!(
            "the server failed to enroll this agent (code {}); see the server's log",
            i32::from(code)
        ),
    }
}

/// A refusal before the server enrolled anything: one line, exit 1.
fn refuse(err: &mut dyn Write, why: &str) -> Exit {
    let _ = writeln!(err, "afkfleet-agent: {why}");
    Exit::Failed
}

/// The server enrolled `agent` and used the token, but this host stored
/// nothing: one `error` and one line, exit 3.
fn lost(err: &mut dyn Write, agent: Option<AgentId>, why: &str) -> Exit {
    let named = agent.map_or_else(|| "an agent".to_owned(), |agent| format!("agent {agent}"));
    error!(agent_id = ?agent, reason = why, "the server enrolled an agent this host didn't store");
    let _ = writeln!(
        err,
        "afkfleet-agent: the server enrolled {named} and used the token, but this host didn't store it: {why}. Disable {named}, then enroll with a new token"
    );
    Exit::Lost
}

/// The request may have reached the server and no clear answer came back
/// (`unclear`): the server may have enrolled the agent `name` and used the
/// token, and this host stored nothing. One `error` and one line, exit 3.
fn unclear_end(err: &mut dyn Write, name: &AgentName, unclear: Unclear) -> Exit {
    let why = match unclear {
        Unclear::TimedOut => "the server didn't answer within 30 s".to_owned(),
        Unclear::Broken(code) => format!(
            "the call ended without the server's answer (code {})",
            i32::from(code)
        ),
        Unclear::Failed => "the server failed; see its log".to_owned(),
    };
    error!(reason = %why, "the server may have enrolled an agent this host didn't store");
    let _ = writeln!(
        err,
        "afkfleet-agent: the server may have enrolled this agent ({name}) and used the token, but no clear answer came back: {why}. If the server has an agent named {name} now, disable it; then enroll with a new token"
    );
    Exit::Lost
}

/// `error` with its causes, as one line.
fn chain(error: &(dyn core::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut next = error.source();
    while let Some(cause) = next {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        next = cause.source();
    }
    text
}
