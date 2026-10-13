//! [`AppState`]: the server profile and the client the commands use (Plan.md
//! P8.4, ADR-0017). No token is ever here: fleet-client holds the access and
//! step tokens, the keychain the refresh token.
//!
//! - **One lock for the connection.** Every command holds a shared guard of
//!   one async `RwLock` for its whole client call, and
//!   [`AppState::set_server_profile`] takes it exclusively, so nothing runs on
//!   the old client after a switch. A command takes the guard exactly once.
//! - **A stored session resumes only under the exact profile it was created
//!   with.** Setting a profile identical to the current one (same origin,
//!   same certificates) changes nothing. Any other profile logs out of the
//!   old server first, then clears the new origin's keychain entry without a
//!   request, since a token stored there came from a profile whose
//!   certificates can't be compared any more.
//! - **The server must answer first.** The new client's `health_live` runs
//!   before the exclusive guard is taken, so a slow or unreachable server
//!   never blocks the other commands, and an error changes nothing.

use std::path::PathBuf;

use fleet_api_types::app::{
    CommandError, CommandErrorCode, LogoutResponse, ProfileUnreadable, ServerProfileRequest,
    ServerProfileResponse, ServerProfileState,
};
use fleet_client::{Client, ClientError};
use garde::Validate as _;
use tauri::async_runtime::RwLock;

use crate::error;
use crate::keychain::StoreFactory;
use crate::profile::{self, Loaded, ServerProfile};

/// The app's state: where the profile lives, the keychain, and the current
/// connection; see the [module docs](self).
pub struct AppState<F: StoreFactory> {
    profile_path: PathBuf,
    factory: F,
    /// The connection: every command reads it for its whole call; only
    /// `set_server_profile` writes it.
    pub(crate) conn: RwLock<Conn<F::Store>>,
}

/// The current connection, or why there's none.
pub(crate) enum Conn<S> {
    /// No server is set up yet.
    NotSet,
    /// The saved profile couldn't be used at startup.
    Unreadable(ProfileUnreadable),
    /// A server is set up.
    Set(Connection<S>),
}

/// A server profile and its client.
pub(crate) struct Connection<S> {
    profile: ServerProfile,
    client: Client<S>,
}

impl<S> Conn<S> {
    /// The client, or `no_server`.
    pub(crate) fn client(&self) -> Result<&Client<S>, CommandError> {
        match self {
            Self::Set(connection) => Ok(&connection.client),
            Self::NotSet | Self::Unreadable(_) => {
                Err(CommandError::new(CommandErrorCode::NoServer))
            }
        }
    }
}

impl<F: StoreFactory> AppState<F> {
    /// The state at startup: the profile saved at `profile_path`, if it can
    /// be used, with its client over `factory`'s store.
    #[must_use]
    pub fn load(profile_path: PathBuf, factory: F) -> Self {
        let conn = match profile::load(&profile_path) {
            Loaded::NotSet => Conn::NotSet,
            Loaded::Unreadable(reason) => Conn::Unreadable(reason),
            Loaded::Set(profile) => match connect(&factory, &profile) {
                Ok(client) => Conn::Set(Connection { profile, client }),
                Err(_) => Conn::Unreadable(ProfileUnreadable::Client),
            },
        };
        Self {
            profile_path,
            factory,
            conn: RwLock::new(conn),
        }
    }

    /// The `server_profile` command: the current profile, or why there's
    /// none.
    pub async fn server_profile(&self) -> ServerProfileState {
        match &*self.conn.read().await {
            Conn::NotSet => ServerProfileState::NotSet,
            Conn::Unreadable(reason) => ServerProfileState::Unreadable { reason: *reason },
            Conn::Set(connection) => ServerProfileState::Set {
                profile: connection.profile.view(),
            },
        }
    }

    /// The `set_server_profile` command; see the [module docs](self).
    ///
    /// # Errors
    /// `invalid_input`, `invalid_server_url` or `invalid_ca` for a refused
    /// request; `store` or `client_build` when its client can't be built; the
    /// health check's error. All of these change nothing. After the old
    /// server's logout, `session_discard` when the new origin's entry can't
    /// be cleared and `profile_save` when the file can't be written: the old
    /// profile stays, its session logged out. Those two codes mean only that,
    /// so the UI can tell a switch that logged out from one that changed
    /// nothing.
    pub async fn set_server_profile(
        &self,
        request: ServerProfileRequest,
    ) -> Result<ServerProfileResponse, CommandError> {
        // Outside the exclusive guard: nothing here touches shared state, and
        // a slow server must not block the other commands.
        request
            .validate()
            .map_err(|report| error::invalid(&report))?;
        let profile = ServerProfile::parse(&request)?;
        let client = connect(&self.factory, &profile)?;
        client.health_live().await.map_err(error::client)?;

        let mut conn = self.conn.write().await;
        if let Conn::Set(current) = &*conn
            && current.profile.same_as(&profile)
        {
            return Ok(ServerProfileResponse::Unchanged {
                profile: current.profile.view(),
            });
        }
        // The old session ends first: for the same origin with another CA,
        // its logout still finds its token to revoke with.
        let logout = match &*conn {
            Conn::Set(current) => logout_response(current.client.logout().await),
            Conn::NotSet | Conn::Unreadable(_) => LogoutResponse::Ended,
        };
        // A token stored for the new origin came from a profile whose
        // certificates can't be compared any more: dropped unsent.
        client
            .discard_session()
            .await
            .map_err(|_| error::session_discard())?;
        profile::save(&self.profile_path, &profile)
            .await
            .map_err(|_| error::profile_save())?;
        let view = profile.view();
        *conn = Conn::Set(Connection { profile, client });
        Ok(ServerProfileResponse::Switched {
            profile: view,
            logout,
        })
    }

    /// The `check_server` command: whether the current server can serve
    /// requests (`health_ready`).
    ///
    /// # Errors
    /// `no_server`, or the health check's error, e.g. `unreachable` or a 503
    /// `busy` as `api` with its `retry_after_secs`.
    pub async fn check_server(&self) -> Result<(), CommandError> {
        let conn = self.conn.read().await;
        conn.client()?.health_ready().await.map_err(error::client)
    }
}

/// A client for `profile` over its origin's store.
fn connect<F: StoreFactory>(
    factory: &F,
    profile: &ServerProfile,
) -> Result<Client<F::Store>, CommandError> {
    let store = factory
        .for_origin(profile.url())
        .map_err(|failure| error::store(&failure))?;
    Client::new(profile.config(), store).map_err(|failure| error::build(&failure))
}

/// A logout's outcome as the UI sees it: the session has ended in the app
/// either way.
pub(crate) fn logout_response(result: Result<(), ClientError>) -> LogoutResponse {
    match result {
        Ok(()) => LogoutResponse::Ended,
        Err(failure) => LogoutResponse::Unconfirmed {
            error: error::client(failure),
        },
    }
}

/// Only the profile's path; never the connection.
impl<F: StoreFactory> core::fmt::Debug for AppState<F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AppState")
            .field("profile_path", &self.profile_path)
            .finish_non_exhaustive()
    }
}
