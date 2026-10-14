//! Microsoft's requests and answers as JSON (ADR-0018): azalea-auth 0.16.0's
//! fields, written and read by serde.
//!
//! The answers keep only the fields the adapter needs, all optional, so a
//! missing one becomes a named refusal instead of a parse error, and fields
//! Microsoft adds are ignored. A token is read straight into a
//! [`SecretString`] ([`WireSecret`]), so no plain copy outlives the parse.
//! None of these types has a `Debug`: they never reach a log.

use secrecy::SecretString;
use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer, Serialize};

/// A token as it arrives, wrapped the moment it's read.
pub(super) struct WireSecret(pub(super) SecretString);

impl<'de> Deserialize<'de> for WireSecret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|text| Self(SecretString::from(text)))
    }
}

/// The device-code answer (`oauth20_connect.srf`).
#[derive(Deserialize)]
pub(super) struct DeviceAnswer {
    pub(super) device_code: Option<WireSecret>,
    pub(super) user_code: Option<String>,
    pub(super) verification_uri: Option<String>,
    pub(super) expires_in: Option<u64>,
    pub(super) interval: Option<u64>,
}

/// A token answer (`oauth20_token.srf`), for the poll and the refresh.
#[derive(Deserialize)]
pub(super) struct TokenAnswer {
    pub(super) access_token: Option<WireSecret>,
    pub(super) refresh_token: Option<WireSecret>,
    pub(super) expires_in: Option<u64>,
}

/// An OAuth error (`oauth20_token.srf` with 400 or 401).
#[derive(Deserialize)]
pub(super) struct OAuthErrorAnswer {
    pub(super) error: Option<String>,
}

/// Xbox Live's and XSTS's answer.
#[derive(Deserialize)]
pub(super) struct XboxAnswer {
    #[serde(rename = "Token")]
    pub(super) token: Option<WireSecret>,
    #[serde(rename = "DisplayClaims")]
    pub(super) display_claims: Option<DisplayClaims>,
}

/// The claims of an Xbox answer.
#[derive(Deserialize)]
pub(super) struct DisplayClaims {
    pub(super) xui: Option<Vec<UserClaims>>,
}

/// One user's claims.
#[derive(Deserialize)]
pub(super) struct UserClaims {
    pub(super) uhs: Option<String>,
}

/// XSTS's refusal (401).
#[derive(Deserialize)]
pub(super) struct XstsRefusal {
    #[serde(rename = "XErr")]
    pub(super) xerr: Option<u64>,
}

/// The Minecraft login's answer.
#[derive(Deserialize)]
pub(super) struct MinecraftLoginAnswer {
    pub(super) access_token: Option<WireSecret>,
    pub(super) expires_in: Option<u64>,
}

/// The entitlements' answer: only how many items there are matters.
#[derive(Deserialize)]
pub(super) struct EntitlementsAnswer {
    pub(super) items: Option<Vec<IgnoredAny>>,
}

/// The profile's answer.
#[derive(Deserialize)]
pub(super) struct ProfileAnswer {
    pub(super) id: Option<String>,
    pub(super) name: Option<String>,
}

/// The profile's 404: `NOT_FOUND` in `error` or `errorType`.
#[derive(Deserialize)]
pub(super) struct NotFoundAnswer {
    pub(super) error: Option<String>,
    #[serde(rename = "errorType")]
    pub(super) error_type: Option<String>,
}

/// Xbox Live's user authentication request.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct XboxUserRequest<'a> {
    pub(super) properties: XboxUserProperties<'a>,
    pub(super) relying_party: &'static str,
    pub(super) token_type: &'static str,
}

/// Its properties: the Microsoft access token as an RPS ticket, as azalea
/// sends it (without a `d=` prefix).
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct XboxUserProperties<'a> {
    pub(super) auth_method: &'static str,
    pub(super) site_name: &'static str,
    pub(super) rps_ticket: &'a str,
}

/// XSTS's authorization request.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct XstsRequest<'a> {
    pub(super) properties: XstsProperties<'a>,
    pub(super) relying_party: &'static str,
    pub(super) token_type: &'static str,
}

/// Its properties: the Xbox Live token.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct XstsProperties<'a> {
    pub(super) sandbox_id: &'static str,
    pub(super) user_tokens: [&'a str; 1],
}

/// Minecraft's login request.
#[derive(Serialize)]
pub(super) struct MinecraftLoginRequest<'a> {
    #[serde(rename = "identityToken")]
    pub(super) identity_token: &'a str,
}
