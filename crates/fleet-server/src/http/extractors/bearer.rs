//! The one `Authorization: Bearer` header of a request, as a secret
//! (Plan.md P7.8, ADR-0016): what [`AuthUser`](super::AuthUser) and
//! [`SetupToken`](super::SetupToken) read their token from.
//!
//! Exactly one `Authorization` header must be there: it isn't a list
//! header, and a proxy and the server must not disagree about which one
//! counts. headers 0.4's `Bearer` matches the scheme in any case and trims
//! the spaces after it (RFC 6750's `"Bearer" 1*SP b64token`); the token's
//! own shape check does the rest. Its `Debug` would show the token, so only
//! its text goes on, as a [`SecretString`].

use axum::extract::FromRequestParts as _;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum_extra::TypedHeader;
use axum_extra::headers::Authorization;
use axum_extra::headers::authorization::Bearer;
use secrecy::SecretString;

/// The request's bearer token, or why there's none, as the log names it.
///
/// # Errors
/// The reason: no header, more than one, or another scheme.
pub(super) async fn bearer<S: Send + Sync>(
    parts: &mut Parts,
    state: &S,
) -> Result<SecretString, &'static str> {
    match parts.headers.get_all(AUTHORIZATION).iter().count() {
        0 => return Err("no Authorization header"),
        1 => {}
        _ => return Err("more than one Authorization header"),
    }
    let Ok(TypedHeader(Authorization(bearer))) =
        TypedHeader::<Authorization<Bearer>>::from_request_parts(parts, state).await
    else {
        return Err("not a Bearer credential");
    };
    let presented = SecretString::from(bearer.token());
    drop(bearer);
    Ok(presented)
}
