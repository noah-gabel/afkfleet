//! A session grant's credentials. The Minecraft access token leaves the
//! generated message for a `SecretString` at once, and the message's copy is
//! wiped (ADR-0019).

use core::mem;

use fleet_core::mc::SessionCredentials;
use fleet_core::value::{McUsername, McUuid};
use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroize;

use super::{time_from_wire, time_to_wire, uuid_from_wire};
use crate::generated::session_grant::Session;
use crate::generated::{OfflineSession, OnlineSession};
use crate::{ConvertError, Problem};

/// The longest access token the wire accepts: 16 KiB, the bound fleet-server's
/// Microsoft adapter puts on every token (ADR-0018).
pub(crate) const MAX_TOKEN_LEN: usize = 16 * 1024;

/// Writes credentials for a session grant.
pub(crate) fn session_to_wire(credentials: SessionCredentials) -> Result<Session, ConvertError> {
    Ok(match credentials {
        SessionCredentials::Offline { username } => Session::Offline(OfflineSession {
            username: username.into(),
        }),
        SessionCredentials::Online {
            username,
            uuid,
            access_token,
            expires_at,
        } => Session::Online(OnlineSession {
            username: username.into(),
            uuid: uuid.as_bytes().to_vec(),
            access_token: access_token.expose_secret().to_owned(),
            expires_at_unix_ms: Some(time_to_wire(
                "OnlineSession.expires_at_unix_ms",
                expires_at,
            )?),
        }),
    })
}

/// Reads a session grant's credentials.
pub(crate) fn session_from_wire(session: Session) -> Result<SessionCredentials, ConvertError> {
    match session {
        Session::Offline(offline) => Ok(SessionCredentials::Offline {
            username: username_from_wire("OfflineSession.username", &offline.username)?,
        }),
        Session::Online(mut online) => {
            const UUID: &str = "OnlineSession.uuid";
            // The token leaves the message first, so every path below drops it
            // as a `SecretString`; the message's `Drop` wipes what's left.
            let access_token = token_from_wire(&mut online.access_token)?;
            let username = username_from_wire("OnlineSession.username", &online.username)?;
            let uuid = uuid_from_wire(UUID, &online.uuid)?;
            McUuid::try_from(uuid).map_err(|_| ConvertError::new(UUID, Problem::Nil))?;
            let expires_at = time_from_wire(
                "OnlineSession.expires_at_unix_ms",
                online.expires_at_unix_ms,
            )?;
            Ok(SessionCredentials::Online {
                username,
                uuid,
                access_token,
                expires_at,
            })
        }
    }
}

fn username_from_wire(field: &'static str, name: &str) -> Result<McUsername, ConvertError> {
    McUsername::try_from(name).map_err(|e| ConvertError::new(field, e.into()))
}

/// Moves the token out of the message into a `SecretString` and wipes the
/// message's copy: 1 byte to 16 KiB of visible ASCII, as Minecraft's tokens are.
fn token_from_wire(raw: &mut String) -> Result<SecretString, ConvertError> {
    let mut raw = mem::take(raw);
    let valid =
        (1..=MAX_TOKEN_LEN).contains(&raw.len()) && raw.bytes().all(|byte| byte.is_ascii_graphic());
    // `from(&str)` allocates exactly the token's length, so no copy is left in
    // a buffer that grew.
    let token = valid.then(|| SecretString::from(raw.as_str()));
    raw.zeroize();
    token.ok_or(ConvertError::new(
        "OnlineSession.access_token",
        Problem::InvalidToken,
    ))
}
