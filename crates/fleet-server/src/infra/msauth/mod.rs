//! [`ReqwestMicrosoftAuth`]: the [`MicrosoftAuthProvider`] over Microsoft's,
//! Xbox Live's and Minecraft's HTTP APIs (Plan.md P9.3, ADR-0018).
//!
//! It sends every request itself, with the protocol of azalea-auth 0.16.0:
//! its client ID, scope, form fields, JSON bodies and headers. azalea-auth
//! itself isn't used:
//! - it checks no status, so `invalid_grant`, a 429, an Xbox refusal and a
//!   missing profile look alike
//! - its only poll is a loop that drops every refusal
//! - it can panic on an answer it doesn't expect
//! - its types print tokens in `Debug`
//!
//! deny.toml keeps it out of fleet-server (ADR-0018).
//!
//! Its private modules:
//! - `client`: the one client builder, the capped reader and what a reqwest
//!   error means.
//! - `wire`: the requests and answers as JSON.
//! - `answers`: what an answer means, as pure functions.
//!
//! Every call reads the clock just before the request whose `expires_in` it
//! uses, so a computed expiry is never later than the real one. Each call
//! is bounded as a whole and sends each request once: the callers own
//! retries and backoff. A failed call logs one `debug` line naming the
//! endpoint, the status and the rule it broke, never a body, a header or
//! reqwest's error text. A successful call logs nothing.

mod answers;
mod client;
mod wire;

use core::fmt;
use core::future::Future;
use core::time::Duration;
use std::sync::Arc;

use async_trait::async_trait;
use fleet_core::system::Clock;
use reqwest::RequestBuilder;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use secrecy::ExposeSecret as _;
use serde::Serialize;
use tracing::debug;
use zeroize::Zeroizing;

use self::answers::Answer;
pub use self::client::BuildError;
use self::client::{HttpClient, Timeouts};
use self::wire::{
    MinecraftLoginRequest, XboxUserProperties, XboxUserRequest, XstsProperties, XstsRequest,
};
use crate::ports::msauth::{
    DeviceCode, DeviceFlow, Endpoint, Failure, MicrosoftAuthProvider, MicrosoftTokens,
    MinecraftSession, MsAccessToken, PollOutcome, RefreshError, RefreshToken, Rule, SessionError,
};

/// azalea-auth's default client ID, Nintendo Switch's, which works for
/// accounts under 18. A custom Azure app would need Mojang's approval
/// (Plan.md P9.3).
const CLIENT_ID: &str = "00000000441cc96b";
/// azalea-auth's default scope.
const SCOPE: &str = "service::user.auth.xboxlive.com::MBI_SSL";
/// The device-code grant (RFC 8628).
const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// The content type of the JSON requests.
const JSON: &str = "application/json";
/// The message of the one log line a failed call writes.
const FAILED: &str = "a Microsoft call failed";

/// Where each request goes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Endpoints {
    device_code: String,
    token: String,
    xbox_user: String,
    xsts: String,
    minecraft_login: String,
    entitlements: String,
    profile: String,
}

impl Endpoints {
    /// The real services, azalea-auth 0.16.0's URLs.
    fn microsoft() -> Self {
        Self {
            device_code: "https://login.live.com/oauth20_connect.srf".to_owned(),
            token: "https://login.live.com/oauth20_token.srf".to_owned(),
            xbox_user: "https://user.auth.xboxlive.com/user/authenticate".to_owned(),
            xsts: "https://xsts.auth.xboxlive.com/xsts/authorize".to_owned(),
            minecraft_login: "https://api.minecraftservices.com/authentication/login_with_xbox"
                .to_owned(),
            entitlements: "https://api.minecraftservices.com/entitlements/mcstore".to_owned(),
            profile: "https://api.minecraftservices.com/minecraft/profile".to_owned(),
        }
    }

    /// The same paths on `base`, the tests' fake.
    #[cfg(test)]
    fn at(base: &str) -> Self {
        Self {
            device_code: format!("{base}/oauth20_connect.srf"),
            token: format!("{base}/oauth20_token.srf"),
            xbox_user: format!("{base}/user/authenticate"),
            xsts: format!("{base}/xsts/authorize"),
            minecraft_login: format!("{base}/authentication/login_with_xbox"),
            entitlements: format!("{base}/entitlements/mcstore"),
            profile: format!("{base}/minecraft/profile"),
        }
    }
}

/// The Microsoft adapter; see the [module docs](self).
pub struct ReqwestMicrosoftAuth {
    client: HttpClient,
    endpoints: Endpoints,
    timeouts: Timeouts,
    clock: Arc<dyn Clock>,
}

/// Nothing but the name: no URL, and no token ever lives in it.
impl fmt::Debug for ReqwestMicrosoftAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReqwestMicrosoftAuth")
            .finish_non_exhaustive()
    }
}

impl ReqwestMicrosoftAuth {
    /// The adapter for the real services, https only, with expiries from
    /// `clock`.
    ///
    /// # Errors
    /// [`BuildError`] if reqwest can't set up TLS.
    pub fn new(clock: Arc<dyn Clock>) -> Result<Self, BuildError> {
        Self::build(Endpoints::microsoft(), Timeouts::MICROSOFT, true, clock)
    }

    /// The adapter for the tests' fake: `endpoints` on plain http, with
    /// shorter `timeouts`. It exists only in test builds.
    #[cfg(test)]
    fn with_endpoints(
        endpoints: Endpoints,
        timeouts: Timeouts,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, BuildError> {
        Self::build(endpoints, timeouts, false, clock)
    }

    fn build(
        endpoints: Endpoints,
        timeouts: Timeouts,
        https_only: bool,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, BuildError> {
        Ok(Self {
            client: client::build(&timeouts, https_only)?,
            endpoints,
            timeouts,
            clock,
        })
    }

    /// Sends `request` to `endpoint`, reads the answer and maps it with
    /// `map`. Any failure on the way logs the call's one line.
    async fn exchange<T, E>(
        &self,
        endpoint: Endpoint,
        request: RequestBuilder,
        map: impl FnOnce(&Answer) -> Result<T, E> + Send,
    ) -> Result<T, E>
    where
        E: From<Failure> + fmt::Display,
    {
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => return Err(failed(endpoint, None, client::classify(&error).into())),
        };
        let status = response.status().as_u16();
        let answer = match client::read(endpoint, response).await {
            Ok(answer) => answer,
            Err(failure) => return Err(failed(endpoint, Some(status), failure.into())),
        };
        map(&answer).map_err(|error| failed(endpoint, Some(status), error))
    }
}

/// Logs a failed call's one line and returns `error`.
fn failed<E: fmt::Display>(endpoint: Endpoint, status: Option<u16>, error: E) -> E {
    debug!(endpoint = %endpoint, status, reason = %error, "{FAILED}");
    error
}

/// Runs `call` within `bound`. Running out of time drops the request in
/// flight and logs the call's one line.
async fn bounded<T, E: From<Failure>>(
    name: &'static str,
    bound: Duration,
    call: impl Future<Output = Result<T, E>> + Send,
) -> Result<T, E> {
    if let Ok(result) = tokio::time::timeout(bound, call).await {
        result
    } else {
        debug!(call = name, reason = %Failure::TimedOut, "{FAILED}");
        Err(Failure::TimedOut.into())
    }
}

/// `value` as a JSON body. Serializing these request types can't fail: they
/// hold only text.
fn json_body<T: Serialize>(endpoint: Endpoint, value: &T) -> Result<Vec<u8>, Failure> {
    serde_json::to_vec(value).map_err(|_| Failure::Unexpected {
        endpoint,
        rule: Rule::NotJson,
    })
}

#[async_trait]
impl MicrosoftAuthProvider for ReqwestMicrosoftAuth {
    async fn start_device_flow(&self) -> Result<DeviceFlow, Failure> {
        bounded("start_device_flow", self.timeouts.call, async {
            let now = self.clock.now();
            let request = self.client.post(&self.endpoints.device_code).form(&[
                ("scope", SCOPE),
                ("client_id", CLIENT_ID),
                ("response_type", "device_code"),
            ]);
            self.exchange(Endpoint::DeviceCode, request, |answer| {
                answers::device_flow(answer, now)
            })
            .await
        })
        .await
    }

    async fn poll(&self, code: &DeviceCode) -> Result<PollOutcome, Failure> {
        bounded("poll", self.timeouts.call, async {
            let now = self.clock.now();
            let request = self
                .client
                .post(format!("{}?client_id={CLIENT_ID}", self.endpoints.token))
                .form(&[
                    ("client_id", CLIENT_ID),
                    ("device_code", code.expose_secret()),
                    ("grant_type", DEVICE_CODE_GRANT),
                ]);
            self.exchange(Endpoint::Poll, request, |answer| answers::poll(answer, now))
                .await
        })
        .await
    }

    async fn refresh(&self, token: &RefreshToken) -> Result<MicrosoftTokens, RefreshError> {
        bounded("refresh", self.timeouts.call, async {
            let now = self.clock.now();
            let request = self.client.post(&self.endpoints.token).form(&[
                ("scope", SCOPE),
                ("client_id", CLIENT_ID),
                ("grant_type", "refresh_token"),
                ("refresh_token", token.expose_secret()),
            ]);
            self.exchange(Endpoint::Refresh, request, |answer| {
                answers::refresh(answer, now, token)
            })
            .await
        })
        .await
    }

    async fn minecraft_session(
        &self,
        access: &MsAccessToken,
    ) -> Result<MinecraftSession, SessionError> {
        bounded("minecraft_session", self.timeouts.session, async {
            let body = json_body(
                Endpoint::XboxUser,
                &XboxUserRequest {
                    properties: XboxUserProperties {
                        auth_method: "RPS",
                        site_name: "user.auth.xboxlive.com",
                        rps_ticket: access.expose_secret(),
                    },
                    relying_party: "http://auth.xboxlive.com",
                    token_type: "JWT",
                },
            )?;
            let request = self
                .client
                .post(&self.endpoints.xbox_user)
                .header(CONTENT_TYPE, JSON)
                .header(ACCEPT, JSON)
                .header("x-xbl-contract-version", "1")
                .body(body);
            let xbox = self
                .exchange(Endpoint::XboxUser, request, answers::xbox_user)
                .await?;

            let body = json_body(
                Endpoint::Xsts,
                &XstsRequest {
                    properties: XstsProperties {
                        sandbox_id: "RETAIL",
                        user_tokens: [xbox.token.expose_secret()],
                    },
                    relying_party: "rp://api.minecraftservices.com/",
                    token_type: "JWT",
                },
            )?;
            let request = self
                .client
                .post(&self.endpoints.xsts)
                .header(CONTENT_TYPE, JSON)
                .header(ACCEPT, JSON)
                .body(body);
            let xsts = self
                .exchange(Endpoint::Xsts, request, answers::xsts)
                .await?;

            let identity = Zeroizing::new(format!(
                "XBL3.0 x={};{}",
                xbox.user_hash,
                xsts.expose_secret()
            ));
            let body = json_body(
                Endpoint::MinecraftLogin,
                &MinecraftLoginRequest {
                    identity_token: &identity,
                },
            )?;
            let now = self.clock.now();
            let request = self
                .client
                .post(&self.endpoints.minecraft_login)
                .header(CONTENT_TYPE, JSON)
                .header(ACCEPT, JSON)
                .body(body);
            let (token, expires_at) = self
                .exchange(Endpoint::MinecraftLogin, request, |answer| {
                    answers::minecraft_login(answer, now)
                })
                .await?;

            let request = self
                .client
                .get(&self.endpoints.entitlements)
                .bearer_auth(token.expose_secret());
            self.exchange(Endpoint::Entitlements, request, answers::entitlements)
                .await?;

            let request = self
                .client
                .get(&self.endpoints.profile)
                .bearer_auth(token.expose_secret());
            let (uuid, name) = self
                .exchange(Endpoint::Profile, request, answers::profile)
                .await?;

            Ok(MinecraftSession {
                token,
                expires_at,
                uuid,
                name,
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests;
