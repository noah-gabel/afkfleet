//! What an answer means: the adapter's pure half (ADR-0018). Each function
//! takes the status and the body the capped reader returned, plus the time
//! read before the request, and maps them to the port's types.
//!
//! Nothing here keeps a value it hasn't checked. Numbers are bounded before
//! anything waits on them. Tokens, codes, names and UUIDs go through the
//! port's and fleet-core's constructors. A refusal names the endpoint and
//! the rule, never the value.
//!
//! Statuses: 429 is [`Failure::RateLimited`], 5xx is
//! [`Failure::Unavailable`], and any status a call doesn't expect (a
//! redirect included) is [`Rule::Status`].

use core::fmt;
use core::ops::RangeInclusive;
use core::time::Duration;

use chrono::{DateTime, Utc};
use fleet_core::time::add;
use fleet_core::value::{McUsername, McUuid};
use secrecy::{ExposeSecret as _, SecretString};
use serde::de::DeserializeOwned;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::wire::{
    DeviceAnswer, EntitlementsAnswer, MinecraftLoginAnswer, NotFoundAnswer, OAuthErrorAnswer,
    ProfileAnswer, TokenAnswer, WireSecret, XboxAnswer, XstsRefusal,
};
use crate::ports::msauth::{
    DeviceCode, DeviceFlow, Endpoint, Failure, McToken, MicrosoftTokens, MsAccessToken, OAuthCode,
    PollInterval, PollOutcome, RefreshError, RefreshToken, Rule, SessionError, TokenError,
    UserCode, VerificationUri, XboxRefusal, check_token,
};

/// A device code's lifetime, in seconds (ADR-0018).
const DEVICE_LIFETIME: RangeInclusive<u64> = 60..=3_600;
/// A Microsoft access token's lifetime, in seconds: 60 s to a day.
const ACCESS_LIFETIME: RangeInclusive<u64> = 60..=86_400;
/// A Minecraft token's lifetime, in seconds: 60 s to two days.
const MINECRAFT_LIFETIME: RangeInclusive<u64> = 60..=172_800;
/// The longest Xbox user hash accepted.
const MAX_USER_HASH: usize = 64;

/// An answer as the capped reader returned it. The body may hold tokens, so
/// it's wiped when dropped.
pub(super) struct Answer {
    /// The HTTP status.
    pub(super) status: u16,
    /// The whole body, at most [`MAX_ANSWER_BYTES`](crate::ports::msauth::MAX_ANSWER_BYTES).
    pub(super) body: Zeroizing<Vec<u8>>,
}

/// Xbox Live's user token and the user hash, for the XSTS step and the
/// Minecraft login.
pub(super) struct XboxUser {
    /// The Xbox Live token.
    pub(super) token: SecretString,
    /// The user hash (`uhs`).
    pub(super) user_hash: String,
}

/// Opaque: the token never reaches a log.
impl fmt::Debug for XboxUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XboxUser").finish_non_exhaustive()
    }
}

/// The device-code answer: a started flow.
pub(super) fn device_flow(answer: &Answer, now: DateTime<Utc>) -> Result<DeviceFlow, Failure> {
    let endpoint = Endpoint::DeviceCode;
    if answer.status != 200 {
        return Err(status_failure(endpoint, answer.status));
    }
    let wire: DeviceAnswer = parse(endpoint, &answer.body)?;
    let code = token::<DeviceCode>(endpoint, "device_code", wire.device_code)?;
    let user_code = wire
        .user_code
        .ok_or_else(|| unexpected(endpoint, Rule::Missing("user_code")))?;
    let user_code = UserCode::try_from(user_code.as_str())
        .map_err(|_| unexpected(endpoint, Rule::Malformed("user_code")))?;
    let link = wire
        .verification_uri
        .ok_or_else(|| unexpected(endpoint, Rule::Missing("verification_uri")))?;
    let verification_uri = VerificationUri::try_from(link.as_str())
        .map_err(|_| unexpected(endpoint, Rule::LinkNotAllowed))?;
    let lifetime = lifetime(endpoint, wire.expires_in, &DEVICE_LIFETIME)?;
    let interval = match wire.interval {
        None => PollInterval::DEFAULT,
        Some(secs) => PollInterval::from_secs(secs)
            .map_err(|_| unexpected(endpoint, Rule::OutOfRange("interval")))?,
    };
    Ok(DeviceFlow {
        code,
        user_code,
        verification_uri,
        expires_at: add(now, lifetime),
        interval,
    })
}

/// A poll's answer.
pub(super) fn poll(answer: &Answer, now: DateTime<Utc>) -> Result<PollOutcome, Failure> {
    let endpoint = Endpoint::Poll;
    match answer.status {
        200 => tokens(endpoint, &answer.body, now, None).map(PollOutcome::Authorized),
        400 | 401 => match oauth_code(&answer.body).as_deref() {
            Some("authorization_pending") => Ok(PollOutcome::Pending),
            Some("slow_down") => Ok(PollOutcome::SlowDown),
            Some("authorization_declined" | "access_denied") => Ok(PollOutcome::Declined),
            Some("expired_token" | "bad_verification_code" | "invalid_grant") => {
                Ok(PollOutcome::Expired)
            }
            other => Err(unexpected(endpoint, code_rule(other))),
        },
        status => Err(status_failure(endpoint, status)),
    }
}

/// A refresh's answer. Without a new refresh token, `sent` is kept (RFC 6749
/// §6).
pub(super) fn refresh(
    answer: &Answer,
    now: DateTime<Utc>,
    sent: &RefreshToken,
) -> Result<MicrosoftTokens, RefreshError> {
    let endpoint = Endpoint::Refresh;
    match answer.status {
        200 => Ok(tokens(endpoint, &answer.body, now, Some(sent))?),
        400 | 401 => match oauth_code(&answer.body).as_deref() {
            Some("invalid_grant") => Err(RefreshError::InvalidGrant),
            other => Err(unexpected(endpoint, code_rule(other)).into()),
        },
        status => Err(status_failure(endpoint, status).into()),
    }
}

/// Xbox Live's user authentication answer. Any refusal here is unexpected:
/// the access token just came from Microsoft.
pub(super) fn xbox_user(answer: &Answer) -> Result<XboxUser, Failure> {
    let endpoint = Endpoint::XboxUser;
    if answer.status != 200 {
        return Err(status_failure(endpoint, answer.status));
    }
    let wire: XboxAnswer = parse(endpoint, &answer.body)?;
    let token = xbox_token(endpoint, wire.token)?;
    let user_hash = wire
        .display_claims
        .and_then(|claims| claims.xui)
        .and_then(|users| users.into_iter().next())
        .and_then(|user| user.uhs)
        .ok_or_else(|| unexpected(endpoint, Rule::Missing("uhs")))?;
    let shaped = (1..=MAX_USER_HASH).contains(&user_hash.len())
        && user_hash.bytes().all(|byte| byte.is_ascii_alphanumeric());
    if !shaped {
        return Err(unexpected(endpoint, Rule::Malformed("uhs")));
    }
    Ok(XboxUser { token, user_hash })
}

/// XSTS's answer: its token, or why Xbox Live refused the account.
pub(super) fn xsts(answer: &Answer) -> Result<SecretString, SessionError> {
    let endpoint = Endpoint::Xsts;
    match answer.status {
        200 => {
            let wire: XboxAnswer = parse(endpoint, &answer.body)?;
            Ok(xbox_token(endpoint, wire.token)?)
        }
        401 => {
            let xerr = serde_json::from_slice::<XstsRefusal>(&answer.body)
                .ok()
                .and_then(|refusal| refusal.xerr)
                .and_then(|xerr| u32::try_from(xerr).ok())
                .ok_or_else(|| unexpected(endpoint, Rule::UnreadableXboxError))?;
            Err(SessionError::Xbox(XboxRefusal::from_xerr(xerr)))
        }
        status => Err(status_failure(endpoint, status).into()),
    }
}

/// The Minecraft login's answer: the token and when it expires.
pub(super) fn minecraft_login(
    answer: &Answer,
    now: DateTime<Utc>,
) -> Result<(McToken, DateTime<Utc>), Failure> {
    let endpoint = Endpoint::MinecraftLogin;
    if answer.status != 200 {
        return Err(status_failure(endpoint, answer.status));
    }
    let wire: MinecraftLoginAnswer = parse(endpoint, &answer.body)?;
    let token = token::<McToken>(endpoint, "access_token", wire.access_token)?;
    let lifetime = lifetime(endpoint, wire.expires_in, &MINECRAFT_LIFETIME)?;
    Ok((token, add(now, lifetime)))
}

/// The entitlements' answer: no item at all is [`SessionError::NotOwned`]
/// (azalea's rule).
pub(super) fn entitlements(answer: &Answer) -> Result<(), SessionError> {
    let endpoint = Endpoint::Entitlements;
    if answer.status != 200 {
        return Err(status_failure(endpoint, answer.status).into());
    }
    let wire: EntitlementsAnswer = parse(endpoint, &answer.body)?;
    let items = wire
        .items
        .ok_or_else(|| unexpected(endpoint, Rule::Missing("items")))?;
    if items.is_empty() {
        return Err(SessionError::NotOwned);
    }
    Ok(())
}

/// The profile's answer: its UUID and name, or [`SessionError::NoProfile`].
pub(super) fn profile(answer: &Answer) -> Result<(McUuid, McUsername), SessionError> {
    let endpoint = Endpoint::Profile;
    match answer.status {
        200 => {}
        404 => {
            let not_found =
                serde_json::from_slice::<NotFoundAnswer>(&answer.body).is_ok_and(|wire| {
                    wire.error.as_deref() == Some("NOT_FOUND")
                        || wire.error_type.as_deref() == Some("NOT_FOUND")
                });
            return Err(if not_found {
                SessionError::NoProfile
            } else {
                unexpected(endpoint, Rule::Status(404)).into()
            });
        }
        status => return Err(status_failure(endpoint, status).into()),
    }
    let wire: ProfileAnswer = parse(endpoint, &answer.body)?;
    let id = wire
        .id
        .ok_or_else(|| unexpected(endpoint, Rule::Missing("id")))?;
    let uuid = profile_uuid(&id).ok_or_else(|| unexpected(endpoint, Rule::Malformed("id")))?;
    let name = wire
        .name
        .ok_or_else(|| unexpected(endpoint, Rule::Missing("name")))?;
    let name = McUsername::try_from(name.as_str())
        .map_err(|_| unexpected(endpoint, Rule::Malformed("name")))?;
    Ok((uuid, name))
}

/// A token answer's tokens. `sent`, a refresh's token, stands in for a
/// missing `refresh_token`; without it, a missing one is refused.
fn tokens(
    endpoint: Endpoint,
    body: &[u8],
    now: DateTime<Utc>,
    sent: Option<&RefreshToken>,
) -> Result<MicrosoftTokens, Failure> {
    let wire: TokenAnswer = parse(endpoint, body)?;
    let access = token::<MsAccessToken>(endpoint, "access_token", wire.access_token)?;
    let lifetime = lifetime(endpoint, wire.expires_in, &ACCESS_LIFETIME)?;
    let refresh = match (wire.refresh_token, sent) {
        (Some(new), _) => token::<RefreshToken>(endpoint, "refresh_token", Some(new))?,
        (None, Some(sent)) => sent.clone(),
        (None, None) => return Err(unexpected(endpoint, Rule::Missing("refresh_token"))),
    };
    Ok(MicrosoftTokens {
        access,
        access_expires_at: add(now, lifetime),
        refresh,
    })
}

/// The OAuth error code in a 400 or 401 body, if it has one.
fn oauth_code(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<OAuthErrorAnswer>(body)
        .ok()
        .and_then(|wire| wire.error)
}

/// The rule for an OAuth code a call doesn't map: named when it has a
/// code's shape, unreadable otherwise.
fn code_rule(code: Option<&str>) -> Rule {
    code.and_then(|code| OAuthCode::try_from(code).ok())
        .map_or(Rule::UnreadableOAuthError, Rule::OAuthCode)
}

/// A profile's UUID: exactly 32 hex digits, not the nil UUID.
fn profile_uuid(id: &str) -> Option<McUuid> {
    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Uuid::try_parse(id)
        .ok()
        .and_then(|uuid| McUuid::try_from(uuid).ok())
}

/// The body as `T`.
fn parse<T: DeserializeOwned>(endpoint: Endpoint, body: &[u8]) -> Result<T, Failure> {
    serde_json::from_slice(body).map_err(|_| unexpected(endpoint, Rule::NotJson))
}

/// A token field as the port's type `T`.
fn token<T: TryFrom<SecretString, Error = TokenError>>(
    endpoint: Endpoint,
    field: &'static str,
    value: Option<WireSecret>,
) -> Result<T, Failure> {
    let WireSecret(text) = value.ok_or_else(|| unexpected(endpoint, Rule::Missing(field)))?;
    T::try_from(text).map_err(|_| unexpected(endpoint, Rule::Malformed(field)))
}

/// Xbox Live's or XSTS's `Token`, checked like every token.
fn xbox_token(endpoint: Endpoint, value: Option<WireSecret>) -> Result<SecretString, Failure> {
    let WireSecret(text) = value.ok_or_else(|| unexpected(endpoint, Rule::Missing("Token")))?;
    check_token(text.expose_secret())
        .map_err(|_| unexpected(endpoint, Rule::Malformed("Token")))?;
    Ok(text)
}

/// An `expires_in` within `range`, as a duration.
fn lifetime(
    endpoint: Endpoint,
    expires_in: Option<u64>,
    range: &RangeInclusive<u64>,
) -> Result<Duration, Failure> {
    let secs = expires_in.ok_or_else(|| unexpected(endpoint, Rule::Missing("expires_in")))?;
    if range.contains(&secs) {
        Ok(Duration::from_secs(secs))
    } else {
        Err(unexpected(endpoint, Rule::OutOfRange("expires_in")))
    }
}

/// An answer the port can't use.
const fn unexpected(endpoint: Endpoint, rule: Rule) -> Failure {
    Failure::Unexpected { endpoint, rule }
}

/// A status a call doesn't handle itself: 429, 5xx, or anything else.
const fn status_failure(endpoint: Endpoint, status: u16) -> Failure {
    match status {
        429 => Failure::RateLimited,
        500..=599 => Failure::Unavailable { status },
        _ => unexpected(endpoint, Rule::Status(status)),
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn answer(status: u16, body: &str) -> Answer {
        Answer {
            status,
            body: Zeroizing::new(body.as_bytes().to_vec()),
        }
    }

    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    fn after(secs: u64) -> DateTime<Utc> {
        add(at(), Duration::from_secs(secs))
    }

    fn unexpected(endpoint: Endpoint, rule: Rule) -> Failure {
        Failure::Unexpected { endpoint, rule }
    }

    fn code(text: &str) -> Rule {
        Rule::OAuthCode(OAuthCode::try_from(text).unwrap())
    }

    fn sent() -> RefreshToken {
        RefreshToken::try_from(SecretString::from("sent-refresh-token")).unwrap()
    }

    fn sixty_five() -> String {
        "a".repeat(65)
    }

    // --- Device code -------------------------------------------------------

    const DEVICE: &str = r#"{"user_code":"B7X4KDLQ","device_code":"invented-device-code","verification_uri":"https://www.microsoft.com/link","expires_in":900,"interval":5,"message":"Go to the page"}"#;

    #[test]
    fn a_device_answer_starts_a_flow() {
        let flow = device_flow(&answer(200, DEVICE), at()).unwrap();

        assert_eq!(flow.code.expose_secret(), "invented-device-code");
        assert_eq!(flow.user_code.as_str(), "B7X4KDLQ");
        assert_eq!(
            flow.verification_uri.as_str(),
            "https://www.microsoft.com/link"
        );
        assert_eq!(flow.expires_at, after(900));
        assert_eq!(flow.interval.get(), Duration::from_secs(5));
    }

    #[test]
    fn a_device_answer_without_an_interval_polls_every_five_seconds() {
        let body = r#"{"user_code":"B7X4KDLQ","device_code":"c","verification_uri":"https://www.microsoft.com/link","expires_in":900}"#;

        let flow = device_flow(&answer(200, body), at()).unwrap();

        assert_eq!(flow.interval, PollInterval::DEFAULT);
    }

    #[rstest]
    #[case::shortest(60, 5, Ok(()))]
    #[case::longest(3_600, 5, Ok(()))]
    #[case::too_short(59, 5, Err(Rule::OutOfRange("expires_in")))]
    #[case::too_long(3_601, 5, Err(Rule::OutOfRange("expires_in")))]
    #[case::huge(u64::MAX, 5, Err(Rule::OutOfRange("expires_in")))]
    #[case::interval_1(900, 1, Ok(()))]
    #[case::interval_60(900, 60, Ok(()))]
    #[case::interval_0(900, 0, Err(Rule::OutOfRange("interval")))]
    #[case::interval_61(900, 61, Err(Rule::OutOfRange("interval")))]
    #[case::interval_huge(900, u64::MAX, Err(Rule::OutOfRange("interval")))]
    fn a_device_answers_numbers_are_bounded(
        #[case] expires_in: u64,
        #[case] interval: u64,
        #[case] expected: Result<(), Rule>,
    ) {
        let body = format!(
            r#"{{"user_code":"B7X4KDLQ","device_code":"c","verification_uri":"https://www.microsoft.com/link","expires_in":{expires_in},"interval":{interval}}}"#
        );

        let result = device_flow(&answer(200, &body), at());

        match expected {
            Ok(()) => {
                let flow = result.unwrap();
                assert_eq!(flow.expires_at, after(expires_in));
                assert_eq!(flow.interval.get(), Duration::from_secs(interval));
            }
            Err(rule) => assert_eq!(result.unwrap_err(), unexpected(Endpoint::DeviceCode, rule)),
        }
    }

    #[rstest]
    #[case::device_code("device_code", Rule::Missing("device_code"))]
    #[case::user_code("user_code", Rule::Missing("user_code"))]
    #[case::verification_uri("verification_uri", Rule::Missing("verification_uri"))]
    #[case::expires_in("expires_in", Rule::Missing("expires_in"))]
    fn a_device_answer_missing_a_field_is_refused(#[case] field: &str, #[case] rule: Rule) {
        let mut json: serde_json::Value = serde_json::from_str(DEVICE).unwrap();
        json.as_object_mut().unwrap().remove(field);

        let error = device_flow(&answer(200, &json.to_string()), at()).unwrap_err();

        assert_eq!(error, unexpected(Endpoint::DeviceCode, rule));
    }

    #[rstest]
    #[case::device_code("device_code", r#""two words""#, Rule::Malformed("device_code"))]
    #[case::empty_device_code("device_code", r#""""#, Rule::Malformed("device_code"))]
    #[case::user_code("user_code", r#""b7x4kdlq""#, Rule::Malformed("user_code"))]
    #[case::http_link(
        "verification_uri",
        r#""http://www.microsoft.com/link""#,
        Rule::LinkNotAllowed
    )]
    #[case::other_host(
        "verification_uri",
        r#""https://example.com/link""#,
        Rule::LinkNotAllowed
    )]
    #[case::wrong_type("expires_in", r#""900""#, Rule::NotJson)]
    #[case::negative("interval", "-5", Rule::NotJson)]
    fn a_device_answer_with_a_bad_field_is_refused(
        #[case] field: &str,
        #[case] value: &str,
        #[case] rule: Rule,
    ) {
        let mut json: serde_json::Value = serde_json::from_str(DEVICE).unwrap();
        json[field] = serde_json::from_str(value).unwrap();

        let error = device_flow(&answer(200, &json.to_string()), at()).unwrap_err();

        assert_eq!(error, unexpected(Endpoint::DeviceCode, rule));
    }

    #[rstest]
    #[case::not_json(200, "<html>", unexpected(Endpoint::DeviceCode, Rule::NotJson))]
    #[case::empty(200, "", unexpected(Endpoint::DeviceCode, Rule::NotJson))]
    #[case::rate_limited(429, "", Failure::RateLimited)]
    #[case::server_error(503, "", Failure::Unavailable { status: 503 })]
    #[case::last_5xx(599, "", Failure::Unavailable { status: 599 })]
    #[case::bad_request(400, "{}", unexpected(Endpoint::DeviceCode, Rule::Status(400)))]
    #[case::redirect(302, "", unexpected(Endpoint::DeviceCode, Rule::Status(302)))]
    #[case::created(201, DEVICE, unexpected(Endpoint::DeviceCode, Rule::Status(201)))]
    fn a_device_answer_of_another_kind_is_a_failure(
        #[case] status: u16,
        #[case] body: &str,
        #[case] failure: Failure,
    ) {
        assert_eq!(
            device_flow(&answer(status, body), at()).unwrap_err(),
            failure
        );
    }

    // --- Poll --------------------------------------------------------------

    const TOKENS: &str = r#"{"token_type":"bearer","expires_in":3600,"scope":"service::user.auth.xboxlive.com::MBI_SSL","access_token":"invented-access-token","refresh_token":"invented-refresh-token","user_id":"x"}"#;

    fn oauth_error(code: &str) -> String {
        format!(r#"{{"error":"{code}","error_description":"text"}}"#)
    }

    #[test]
    fn an_approved_poll_carries_the_tokens() {
        let outcome = poll(&answer(200, TOKENS), at()).unwrap();

        let PollOutcome::Authorized(tokens) = outcome else {
            panic!("expected the tokens, got {outcome:?}");
        };
        assert_eq!(tokens.access.expose_secret(), "invented-access-token");
        assert_eq!(tokens.refresh.expose_secret(), "invented-refresh-token");
        assert_eq!(tokens.access_expires_at, after(3_600));
    }

    #[rstest]
    #[case::pending(400, "authorization_pending", "Pending")]
    #[case::pending_401(401, "authorization_pending", "Pending")]
    #[case::slow_down(400, "slow_down", "SlowDown")]
    #[case::declined(400, "authorization_declined", "Declined")]
    #[case::access_denied(400, "access_denied", "Declined")]
    #[case::expired(400, "expired_token", "Expired")]
    #[case::bad_code(400, "bad_verification_code", "Expired")]
    #[case::invalid_grant(400, "invalid_grant", "Expired")]
    fn each_known_oauth_code_is_an_outcome(
        #[case] status: u16,
        #[case] error: &str,
        #[case] outcome: &str,
    ) {
        let result = poll(&answer(status, &oauth_error(error)), at()).unwrap();

        assert_eq!(format!("{result:?}"), outcome);
    }

    #[rstest]
    #[case::unknown_code(oauth_error("invalid_request"), code("invalid_request"))]
    #[case::other_characters(oauth_error("Invalid-Request"), Rule::UnreadableOAuthError)]
    #[case::empty_code(oauth_error(""), Rule::UnreadableOAuthError)]
    #[case::too_long(oauth_error(&sixty_five()), Rule::UnreadableOAuthError)]
    #[case::no_error_field(r#"{"error_description":"text"}"#.to_owned(), Rule::UnreadableOAuthError)]
    #[case::null_error(r#"{"error":null}"#.to_owned(), Rule::UnreadableOAuthError)]
    #[case::number_error(r#"{"error":42}"#.to_owned(), Rule::UnreadableOAuthError)]
    #[case::not_json("<html>".to_owned(), Rule::UnreadableOAuthError)]
    fn any_other_oauth_error_is_unexpected_and_named_only_when_it_is_a_code(
        #[case] body: String,
        #[case] rule: Rule,
    ) {
        assert_eq!(
            poll(&answer(400, &body), at()).unwrap_err(),
            unexpected(Endpoint::Poll, rule)
        );
    }

    #[rstest]
    #[case::no_refresh_token("refresh_token", Rule::Missing("refresh_token"))]
    #[case::no_access_token("access_token", Rule::Missing("access_token"))]
    #[case::no_lifetime("expires_in", Rule::Missing("expires_in"))]
    fn an_approved_poll_missing_a_token_field_is_refused(#[case] field: &str, #[case] rule: Rule) {
        let mut json: serde_json::Value = serde_json::from_str(TOKENS).unwrap();
        json.as_object_mut().unwrap().remove(field);

        let error = poll(&answer(200, &json.to_string()), at()).unwrap_err();

        assert_eq!(error, unexpected(Endpoint::Poll, rule));
    }

    #[rstest]
    #[case::shortest(60, true)]
    #[case::a_day(86_400, true)]
    #[case::too_short(59, false)]
    #[case::too_long(86_401, false)]
    fn the_access_tokens_lifetime_is_bounded(#[case] expires_in: u64, #[case] accepted: bool) {
        let mut json: serde_json::Value = serde_json::from_str(TOKENS).unwrap();
        json["expires_in"] = expires_in.into();

        let result = poll(&answer(200, &json.to_string()), at());

        if accepted {
            let Ok(PollOutcome::Authorized(tokens)) = result else {
                panic!("expected the tokens, got {result:?}");
            };
            assert_eq!(tokens.access_expires_at, after(expires_in));
        } else {
            assert_eq!(
                result.unwrap_err(),
                unexpected(Endpoint::Poll, Rule::OutOfRange("expires_in"))
            );
        }
    }

    #[rstest]
    #[case::access("access_token", Rule::Malformed("access_token"))]
    #[case::refresh("refresh_token", Rule::Malformed("refresh_token"))]
    fn a_malformed_token_is_refused(#[case] field: &str, #[case] rule: Rule) {
        let mut json: serde_json::Value = serde_json::from_str(TOKENS).unwrap();
        json[field] = "two words".into();

        assert_eq!(
            poll(&answer(200, &json.to_string()), at()).unwrap_err(),
            unexpected(Endpoint::Poll, rule)
        );
    }

    #[rstest]
    #[case::rate_limited(429, Failure::RateLimited)]
    #[case::server_error(500, Failure::Unavailable { status: 500 })]
    #[case::forbidden(403, unexpected(Endpoint::Poll, Rule::Status(403)))]
    #[case::redirect(307, unexpected(Endpoint::Poll, Rule::Status(307)))]
    fn a_poll_answer_of_another_status_is_a_failure(#[case] status: u16, #[case] failure: Failure) {
        assert_eq!(
            poll(&answer(status, &oauth_error("authorization_pending")), at()).unwrap_err(),
            failure
        );
    }

    // --- Refresh -----------------------------------------------------------

    #[test]
    fn a_refresh_returns_the_new_tokens() {
        let tokens = refresh(&answer(200, TOKENS), at(), &sent()).unwrap();

        assert_eq!(tokens.access.expose_secret(), "invented-access-token");
        assert_eq!(tokens.refresh.expose_secret(), "invented-refresh-token");
        assert_eq!(tokens.access_expires_at, after(3_600));
    }

    #[test]
    fn a_refresh_without_a_new_refresh_token_keeps_the_one_sent() {
        let mut json: serde_json::Value = serde_json::from_str(TOKENS).unwrap();
        json.as_object_mut().unwrap().remove("refresh_token");

        let tokens = refresh(&answer(200, &json.to_string()), at(), &sent()).unwrap();

        assert_eq!(tokens.refresh.expose_secret(), "sent-refresh-token");
        assert_eq!(tokens.access.expose_secret(), "invented-access-token");
    }

    #[rstest]
    #[case::bad_request(400)]
    #[case::unauthorized(401)]
    fn invalid_grant_refuses_the_refresh_token(#[case] status: u16) {
        assert_eq!(
            refresh(
                &answer(status, &oauth_error("invalid_grant")),
                at(),
                &sent()
            )
            .unwrap_err(),
            RefreshError::InvalidGrant
        );
    }

    #[rstest]
    #[case::invalid_client(oauth_error("invalid_client"), code("invalid_client"))]
    #[case::invalid_scope(oauth_error("invalid_scope"), code("invalid_scope"))]
    #[case::pending(oauth_error("authorization_pending"), code("authorization_pending"))]
    #[case::other_characters(oauth_error("Invalid Grant"), Rule::UnreadableOAuthError)]
    #[case::not_json("<html>".to_owned(), Rule::UnreadableOAuthError)]
    fn any_other_refusal_never_reads_as_invalid_grant(#[case] body: String, #[case] rule: Rule) {
        assert_eq!(
            refresh(&answer(400, &body), at(), &sent()).unwrap_err(),
            RefreshError::Failed(unexpected(Endpoint::Refresh, rule))
        );
    }

    #[rstest]
    #[case::rate_limited(429, Failure::RateLimited)]
    #[case::server_error(502, Failure::Unavailable { status: 502 })]
    #[case::not_found(404, unexpected(Endpoint::Refresh, Rule::Status(404)))]
    fn a_refresh_answer_of_another_status_is_a_failure(
        #[case] status: u16,
        #[case] failure: Failure,
    ) {
        assert_eq!(
            refresh(
                &answer(status, &oauth_error("invalid_grant")),
                at(),
                &sent()
            )
            .unwrap_err(),
            RefreshError::Failed(failure)
        );
    }

    #[test]
    fn a_refresh_answer_missing_the_access_token_is_refused() {
        assert_eq!(
            refresh(&answer(200, r#"{"expires_in":3600}"#), at(), &sent()).unwrap_err(),
            RefreshError::Failed(unexpected(Endpoint::Refresh, Rule::Missing("access_token")))
        );
    }

    // --- Xbox Live ---------------------------------------------------------

    const XBOX: &str = r#"{"IssueInstant":"2026-10-14T00:00:00Z","NotAfter":"2026-10-28T00:00:00Z","Token":"invented-xbox-token","DisplayClaims":{"xui":[{"uhs":"1234567890123456789"}]}}"#;

    #[test]
    fn xbox_lives_answer_gives_the_token_and_the_user_hash() {
        let user = xbox_user(&answer(200, XBOX)).unwrap();

        assert_eq!(user.token.expose_secret(), "invented-xbox-token");
        assert_eq!(user.user_hash, "1234567890123456789");
        assert_eq!(format!("{user:?}"), "XboxUser { .. }");
    }

    #[rstest]
    #[case::no_token(r#"{"DisplayClaims":{"xui":[{"uhs":"123"}]}}"#, Rule::Missing("Token"))]
    #[case::bad_token(
        r#"{"Token":"a b","DisplayClaims":{"xui":[{"uhs":"123"}]}}"#,
        Rule::Malformed("Token")
    )]
    #[case::no_claims(r#"{"Token":"t"}"#, Rule::Missing("uhs"))]
    #[case::no_xui(r#"{"Token":"t","DisplayClaims":{}}"#, Rule::Missing("uhs"))]
    #[case::empty_xui(r#"{"Token":"t","DisplayClaims":{"xui":[]}}"#, Rule::Missing("uhs"))]
    #[case::no_uhs(r#"{"Token":"t","DisplayClaims":{"xui":[{}]}}"#, Rule::Missing("uhs"))]
    #[case::empty_uhs(
        r#"{"Token":"t","DisplayClaims":{"xui":[{"uhs":""}]}}"#,
        Rule::Malformed("uhs")
    )]
    #[case::uhs_hyphen(
        r#"{"Token":"t","DisplayClaims":{"xui":[{"uhs":"12-34"}]}}"#,
        Rule::Malformed("uhs")
    )]
    #[case::not_json("<html>", Rule::NotJson)]
    fn xbox_lives_answer_without_a_usable_field_is_refused(#[case] body: &str, #[case] rule: Rule) {
        assert_eq!(
            xbox_user(&answer(200, body)).unwrap_err(),
            unexpected(Endpoint::XboxUser, rule)
        );
    }

    #[test]
    fn a_user_hash_over_64_characters_is_refused() {
        let body = format!(
            r#"{{"Token":"t","DisplayClaims":{{"xui":[{{"uhs":"{}"}}]}}}}"#,
            sixty_five()
        );

        assert_eq!(
            xbox_user(&answer(200, &body)).unwrap_err(),
            unexpected(Endpoint::XboxUser, Rule::Malformed("uhs"))
        );
    }

    #[rstest]
    #[case::refused(401, unexpected(Endpoint::XboxUser, Rule::Status(401)))]
    #[case::bad_request(400, unexpected(Endpoint::XboxUser, Rule::Status(400)))]
    #[case::rate_limited(429, Failure::RateLimited)]
    #[case::server_error(500, Failure::Unavailable { status: 500 })]
    fn any_refusal_from_xbox_live_is_a_failure(#[case] status: u16, #[case] failure: Failure) {
        let body = r#"{"XErr":2148916233}"#;

        assert_eq!(xbox_user(&answer(status, body)).unwrap_err(), failure);
    }

    // --- XSTS --------------------------------------------------------------

    #[test]
    fn xsts_answer_gives_its_token() {
        let token = xsts(&answer(200, XBOX)).unwrap();

        assert_eq!(token.expose_secret(), "invented-xbox-token");
    }

    #[rstest]
    #[case::banned("2148916227", XboxRefusal::Banned)]
    #[case::no_profile("2148916233", XboxRefusal::NoXboxProfile)]
    #[case::country("2148916235", XboxRefusal::CountryUnavailable)]
    #[case::adult("2148916236", XboxRefusal::AdultVerification)]
    #[case::child("2148916238", XboxRefusal::ChildAccount)]
    #[case::other("2148916262", XboxRefusal::Other { xerr: 2_148_916_262 })]
    fn an_xsts_refusal_names_why(#[case] xerr: &str, #[case] refusal: XboxRefusal) {
        let body = format!(
            r#"{{"Identity":"0","XErr":{xerr},"Message":"","Redirect":"https://start.ui.xboxlive.com/AddChildToFamily"}}"#
        );

        assert_eq!(
            xsts(&answer(401, &body)).unwrap_err(),
            SessionError::Xbox(refusal)
        );
    }

    #[rstest]
    #[case::no_xerr(r#"{"Identity":"0"}"#)]
    #[case::string_xerr(r#"{"XErr":"2148916233"}"#)]
    #[case::over_u32(r#"{"XErr":4294967296}"#)]
    #[case::negative(r#"{"XErr":-1}"#)]
    #[case::not_json("")]
    fn an_xsts_refusal_without_a_readable_code_is_unexpected(#[case] body: &str) {
        assert_eq!(
            xsts(&answer(401, body)).unwrap_err(),
            SessionError::Failed(unexpected(Endpoint::Xsts, Rule::UnreadableXboxError))
        );
    }

    #[rstest]
    #[case::forbidden(403, unexpected(Endpoint::Xsts, Rule::Status(403)))]
    #[case::rate_limited(429, Failure::RateLimited)]
    #[case::server_error(503, Failure::Unavailable { status: 503 })]
    fn an_xsts_answer_of_another_status_is_a_failure(
        #[case] status: u16,
        #[case] failure: Failure,
    ) {
        assert_eq!(
            xsts(&answer(status, r#"{"XErr":2148916233}"#)).unwrap_err(),
            SessionError::Failed(failure)
        );
    }

    #[rstest]
    #[case::no_token(r#"{"DisplayClaims":{}}"#, Rule::Missing("Token"))]
    #[case::bad_token(r#"{"Token":""}"#, Rule::Malformed("Token"))]
    #[case::not_json("<html>", Rule::NotJson)]
    fn an_xsts_answer_without_a_usable_token_is_refused(#[case] body: &str, #[case] rule: Rule) {
        assert_eq!(
            xsts(&answer(200, body)).unwrap_err(),
            SessionError::Failed(unexpected(Endpoint::Xsts, rule))
        );
    }

    // --- Minecraft login ---------------------------------------------------

    #[rstest]
    #[case::shortest(60, true)]
    #[case::a_day(86_400, true)]
    #[case::two_days(172_800, true)]
    #[case::too_short(59, false)]
    #[case::too_long(172_801, false)]
    fn the_minecraft_tokens_lifetime_is_bounded(#[case] expires_in: u64, #[case] accepted: bool) {
        let body = format!(
            r#"{{"username":"x","roles":[],"access_token":"invented-mc-token","token_type":"Bearer","expires_in":{expires_in}}}"#
        );

        let result = minecraft_login(&answer(200, &body), at());

        if accepted {
            let (token, expires_at) = result.unwrap();
            assert_eq!(token.expose_secret(), "invented-mc-token");
            assert_eq!(expires_at, after(expires_in));
        } else {
            assert_eq!(
                result.unwrap_err(),
                unexpected(Endpoint::MinecraftLogin, Rule::OutOfRange("expires_in"))
            );
        }
    }

    #[rstest]
    #[case::no_token(r#"{"expires_in":86400}"#, Rule::Missing("access_token"))]
    #[case::bad_token(
        r#"{"access_token":"a\tb","expires_in":86400}"#,
        Rule::Malformed("access_token")
    )]
    #[case::no_lifetime(r#"{"access_token":"t"}"#, Rule::Missing("expires_in"))]
    #[case::not_json("<html>", Rule::NotJson)]
    fn a_minecraft_login_answer_without_a_usable_field_is_refused(
        #[case] body: &str,
        #[case] rule: Rule,
    ) {
        assert_eq!(
            minecraft_login(&answer(200, body), at()).unwrap_err(),
            unexpected(Endpoint::MinecraftLogin, rule)
        );
    }

    #[rstest]
    #[case::refused(401, unexpected(Endpoint::MinecraftLogin, Rule::Status(401)))]
    #[case::rate_limited(429, Failure::RateLimited)]
    #[case::server_error(500, Failure::Unavailable { status: 500 })]
    fn a_minecraft_login_answer_of_another_status_is_a_failure(
        #[case] status: u16,
        #[case] failure: Failure,
    ) {
        assert_eq!(
            minecraft_login(&answer(status, "{}"), at()).unwrap_err(),
            failure
        );
    }

    // --- Entitlements ------------------------------------------------------

    #[test]
    fn any_entitlement_owns_the_game() {
        let body = r#"{"items":[{"name":"product_game_pass_pc","signature":"x"}],"signature":"x","keyId":"1"}"#;

        assert_eq!(entitlements(&answer(200, body)), Ok(()));
    }

    #[rstest]
    #[case::empty(r#"{"items":[],"signature":"x","keyId":"1"}"#, SessionError::NotOwned)]
    #[case::missing(
        r#"{"signature":"x"}"#,
        SessionError::Failed(unexpected(Endpoint::Entitlements, Rule::Missing("items")))
    )]
    #[case::not_a_list(
        r#"{"items":{}}"#,
        SessionError::Failed(unexpected(Endpoint::Entitlements, Rule::NotJson))
    )]
    #[case::rate_limited("", SessionError::Failed(Failure::RateLimited))]
    fn entitlements_without_an_item_or_a_list_are_refused(
        #[case] body: &str,
        #[case] error: SessionError,
    ) {
        let status = if body.is_empty() { 429 } else { 200 };

        assert_eq!(entitlements(&answer(status, body)).unwrap_err(), error);
    }

    #[test]
    fn an_entitlements_answer_of_another_status_is_unexpected() {
        assert_eq!(
            entitlements(&answer(401, "{}")).unwrap_err(),
            SessionError::Failed(unexpected(Endpoint::Entitlements, Rule::Status(401)))
        );
    }

    // --- Profile -----------------------------------------------------------

    #[rstest]
    #[case::lowercase("4f1c9a2e7b3d4e5f8a6b0c1d2e3f4a5b")]
    #[case::uppercase("4F1C9A2E7B3D4E5F8A6B0C1D2E3F4A5B")]
    fn a_profile_gives_its_uuid_and_name(#[case] id: &str) {
        let body = format!(r#"{{"id":"{id}","name":"AfkBot1","skins":[],"capes":[]}}"#);

        let (uuid, name) = profile(&answer(200, &body)).unwrap();

        assert_eq!(uuid.to_string(), "4f1c9a2e-7b3d-4e5f-8a6b-0c1d2e3f4a5b");
        assert_eq!(name.as_str(), "AfkBot1");
    }

    #[rstest]
    #[case::short_id(
        r#"{"id":"4f1c9a2e7b3d4e5f8a6b0c1d2e3f4a5","name":"AfkBot1"}"#,
        Rule::Malformed("id")
    )]
    #[case::long_id(
        r#"{"id":"4f1c9a2e7b3d4e5f8a6b0c1d2e3f4a5bb","name":"AfkBot1"}"#,
        Rule::Malformed("id")
    )]
    #[case::non_hex(
        r#"{"id":"4f1c9a2e7b3d4e5f8a6b0c1d2e3f4a5g","name":"AfkBot1"}"#,
        Rule::Malformed("id")
    )]
    #[case::hyphenated(
        r#"{"id":"4f1c9a2e-7b3d-4e5f-8a6b-0c1d2e3f4a5b","name":"AfkBot1"}"#,
        Rule::Malformed("id")
    )]
    #[case::nil(
        r#"{"id":"00000000000000000000000000000000","name":"AfkBot1"}"#,
        Rule::Malformed("id")
    )]
    #[case::no_id(r#"{"name":"AfkBot1"}"#, Rule::Missing("id"))]
    #[case::short_name(
        r#"{"id":"4f1c9a2e7b3d4e5f8a6b0c1d2e3f4a5b","name":"ab"}"#,
        Rule::Malformed("name")
    )]
    #[case::bad_name(
        r#"{"id":"4f1c9a2e7b3d4e5f8a6b0c1d2e3f4a5b","name":"Afk Bot"}"#,
        Rule::Malformed("name")
    )]
    #[case::no_name(r#"{"id":"4f1c9a2e7b3d4e5f8a6b0c1d2e3f4a5b"}"#, Rule::Missing("name"))]
    #[case::not_json("<html>", Rule::NotJson)]
    fn a_profile_without_a_usable_field_is_refused(#[case] body: &str, #[case] rule: Rule) {
        assert_eq!(
            profile(&answer(200, body)).unwrap_err(),
            SessionError::Failed(unexpected(Endpoint::Profile, rule))
        );
    }

    #[rstest]
    #[case::error(r#"{"path":"/minecraft/profile","error":"NOT_FOUND","errorMessage":"x"}"#)]
    #[case::error_type(r#"{"path":"/minecraft/profile","errorType":"NOT_FOUND","error":"x"}"#)]
    fn a_404_not_found_is_no_profile(#[case] body: &str) {
        assert_eq!(
            profile(&answer(404, body)).unwrap_err(),
            SessionError::NoProfile
        );
    }

    #[rstest]
    #[case::other_error(
        404,
        r#"{"error":"GONE"}"#,
        unexpected(Endpoint::Profile, Rule::Status(404))
    )]
    #[case::not_json(404, "<html>", unexpected(Endpoint::Profile, Rule::Status(404)))]
    #[case::refused(401, "{}", unexpected(Endpoint::Profile, Rule::Status(401)))]
    #[case::rate_limited(429, "{}", Failure::RateLimited)]
    #[case::server_error(500, "{}", Failure::Unavailable { status: 500 })]
    fn a_profile_answer_of_another_kind_is_a_failure(
        #[case] status: u16,
        #[case] body: &str,
        #[case] failure: Failure,
    ) {
        assert_eq!(
            profile(&answer(status, body)).unwrap_err(),
            SessionError::Failed(failure)
        );
    }
}
