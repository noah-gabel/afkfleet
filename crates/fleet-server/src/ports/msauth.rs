//! The Microsoft account port (Plan.md P9.3, ADR-0018):
//! [`MicrosoftAuthProvider`] links an account through Microsoft's
//! device-code flow, refreshes its tokens and mints Minecraft session tokens.
//! The session-token service (P9.5), the link flow (P9.4) and the app's
//! screens (P9.8) see Microsoft only through it.
//!
//! Everything Microsoft sends is input, and these types check it when
//! they're built:
//! - Tokens are 1 B to [`MAX_TOKEN_BYTES`] of visible ASCII. They live only in
//!   [`SecretString`]s, behind an opaque `Debug`.
//! - The user code, the verification link and the poll interval have fixed
//!   shapes and ranges.
//! - The errors name the call and the rule an answer broke, never a value from
//!   it, so they can be logged and shown.
//!
//! Refresh tokens never leave the server (security rule 6). Only
//! [`MinecraftSession`]'s token goes to an agent, through P9.5's service.
//! Device-code phishing, someone else entering a code a user started, is
//! countered by the approval of new links and the link dialog's warning
//! (ADR-0018), not here.
//!
//! [`VerificationUri`] is parsed with reqwest's `Url`, so the adapter and the
//! tests' fake share one rule; no reqwest type appears in the port's API.

use core::fmt;
use core::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::account::RefusalReason;
use fleet_core::value::{McUsername, McUuid};
use secrecy::{ExposeSecret as _, SecretString};

/// The longest token the port accepts, in bytes. It's also the upper bound
/// of a stored refresh token (ADR-0018).
pub const MAX_TOKEN_BYTES: usize = 16 * 1024;

/// The largest answer body the adapter reads, in bytes (ADR-0018).
pub const MAX_ANSWER_BYTES: usize = 256 * 1024;

/// Why a text isn't a valid token. Neither variant carries the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    /// Empty, or longer than [`MAX_TOKEN_BYTES`].
    #[error("a token is 1 to 16384 bytes long")]
    Length,
    /// A byte outside visible ASCII (0x21–0x7E): a space, a control
    /// character or a non-ASCII one.
    #[error("a token holds only visible ASCII characters")]
    Character,
}

/// Checks a token's length and characters: also for the Xbox tokens, which
/// the adapter uses within one call and never returns.
pub(crate) fn check_token(text: &str) -> Result<(), TokenError> {
    if text.is_empty() || text.len() > MAX_TOKEN_BYTES {
        return Err(TokenError::Length);
    }
    if !text.bytes().all(|byte| (0x21..=0x7E).contains(&byte)) {
        return Err(TokenError::Character);
    }
    Ok(())
}

/// A secret token: a [`SecretString`] that passed [`check_token`], with an
/// opaque `Debug`.
macro_rules! token {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone)]
        pub struct $name(SecretString);

        impl $name {
            /// The token's text, for the one request that sends it.
            #[must_use]
            pub fn expose_secret(&self) -> &str {
                self.0.expose_secret()
            }
        }

        impl TryFrom<SecretString> for $name {
            type Error = TokenError;

            fn try_from(text: SecretString) -> Result<Self, TokenError> {
                check_token(text.expose_secret())?;
                Ok(Self(text))
            }
        }

        /// Opaque: the token never reaches a log.
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(..)"))
            }
        }
    };
}

token!(
    /// The device code of a started flow: whoever holds it gets the
    /// account's tokens once its user approves, so it's a secret.
    DeviceCode
);
token!(
    /// A Microsoft refresh token, stored only sealed by the vault and never
    /// sent anywhere but Microsoft (security rule 6).
    RefreshToken
);
token!(
    /// A Microsoft access token, used only to start a Minecraft session.
    MsAccessToken
);
token!(
    /// A Minecraft session token: the only token that leaves the server, to
    /// the agent that runs the account's bot.
    McToken
);

/// Why a text isn't a valid [`UserCode`]. It doesn't carry the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a user code is 4 to 16 of A-Z, 0-9 and -")]
pub struct UserCodeError;

/// The code a user enters at Microsoft's page: 4 to 16 of `A`–`Z`, `0`–`9`
/// and `-`. Shown to the user who started the flow, and never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct UserCode(String);

impl UserCode {
    /// The shortest code accepted.
    pub const MIN_LEN: usize = 4;
    /// The longest code accepted.
    pub const MAX_LEN: usize = 16;

    /// The code, for display.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for UserCode {
    type Error = UserCodeError;

    fn try_from(text: &str) -> Result<Self, UserCodeError> {
        let shaped = (Self::MIN_LEN..=Self::MAX_LEN).contains(&text.len())
            && text
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-');
        if shaped {
            Ok(Self(text.to_owned()))
        } else {
            Err(UserCodeError)
        }
    }
}

/// Opaque: a code someone could still enter never reaches a log.
impl fmt::Debug for UserCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UserCode(..)")
    }
}

/// Why a text isn't a valid [`VerificationUri`]. It doesn't carry the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "a verification link is https to www.microsoft.com or microsoft.com, without a user, another port, a query or a fragment, at most 256 characters"
)]
pub struct VerificationUriError;

/// The page where the user enters the code. The app opens it (P9.8), so
/// only an https link to one of [`VerificationUri::HOSTS`] is accepted:
/// without a user, a port other than 443, a query or a fragment, and at most
/// [`VerificationUri::MAX_LEN`] characters as written back, so the link
/// flow's answer always holds it whole (ADR-0018).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationUri(String);

impl VerificationUri {
    /// The hosts the link may name.
    pub const HOSTS: [&str; 2] = ["www.microsoft.com", "microsoft.com"];

    /// The longest link, in characters (it's ASCII once parsed).
    pub const MAX_LEN: usize = 256;

    /// The link, as parsed and written back.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for VerificationUri {
    type Error = VerificationUriError;

    fn try_from(text: &str) -> Result<Self, VerificationUriError> {
        let url = reqwest::Url::parse(text).map_err(|_| VerificationUriError)?;
        let allowed = url.scheme() == "https"
            && url
                .host_str()
                .is_some_and(|host| Self::HOSTS.contains(&host))
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.as_str().len() <= Self::MAX_LEN;
        if allowed {
            Ok(Self(url.as_str().to_owned()))
        } else {
            Err(VerificationUriError)
        }
    }
}

/// Why a number of seconds isn't a valid [`PollInterval`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a poll interval is 1 to 60 seconds")]
pub struct PollIntervalError;

/// How long to wait between two polls: 1 to 60 seconds (ADR-0018).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollInterval(Duration);

impl PollInterval {
    /// The shortest interval.
    pub const MIN: Duration = Duration::from_secs(1);
    /// The longest interval, which [`PollInterval::slowed_down`] never
    /// passes.
    pub const MAX: Duration = Duration::from_secs(60);
    /// RFC 8628's default, for an answer without an interval.
    pub const DEFAULT: Self = Self(Duration::from_secs(5));
    /// What a `slow_down` adds (RFC 8628 §3.5).
    pub const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);

    /// An interval of `secs` seconds.
    ///
    /// # Errors
    /// [`PollIntervalError`] outside 1 to 60 seconds.
    pub const fn from_secs(secs: u64) -> Result<Self, PollIntervalError> {
        if secs < Self::MIN.as_secs() || secs > Self::MAX.as_secs() {
            return Err(PollIntervalError);
        }
        Ok(Self(Duration::from_secs(secs)))
    }

    /// The interval.
    #[must_use]
    pub const fn get(self) -> Duration {
        self.0
    }

    /// The interval after a `slow_down`: [`PollInterval::SLOW_DOWN_STEP`]
    /// longer, at most [`PollInterval::MAX`].
    #[must_use]
    pub fn slowed_down(self) -> Self {
        Self(self.0.saturating_add(Self::SLOW_DOWN_STEP).min(Self::MAX))
    }
}

/// Why a text isn't a valid [`OAuthCode`]. It doesn't carry the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("an OAuth code is 1 to 64 of a-z and _")]
pub struct OAuthCodeError;

/// An OAuth error code Microsoft sent that the port doesn't map, kept for
/// [`Rule::OAuthCode`]: 1 to 64 of `a`–`z` and `_`. Codes are protocol words,
/// not secrets, and the strict shape keeps anything else out (ADR-0018).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthCode(String);

impl OAuthCode {
    /// The longest code kept.
    pub const MAX_LEN: usize = 64;

    /// The code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for OAuthCode {
    type Error = OAuthCodeError;

    fn try_from(text: &str) -> Result<Self, OAuthCodeError> {
        let shaped = (1..=Self::MAX_LEN).contains(&text.len())
            && text
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_');
        if shaped {
            Ok(Self(text.to_owned()))
        } else {
            Err(OAuthCodeError)
        }
    }
}

impl fmt::Display for OAuthCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The request an [`Failure::Unexpected`] answer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    /// Starting a device-code flow.
    DeviceCode,
    /// Polling a device code.
    Poll,
    /// Refreshing the Microsoft tokens.
    Refresh,
    /// Xbox Live's user authentication.
    XboxUser,
    /// Xbox Live's XSTS authorization for Minecraft.
    Xsts,
    /// Minecraft's login with the Xbox identity.
    MinecraftLogin,
    /// The account's Minecraft entitlements.
    Entitlements,
    /// The account's Minecraft profile.
    Profile,
}

impl Endpoint {
    /// The request's name in errors and logs.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::DeviceCode => "device code",
            Self::Poll => "poll",
            Self::Refresh => "refresh",
            Self::XboxUser => "xbox user",
            Self::Xsts => "xsts",
            Self::MinecraftLogin => "minecraft login",
            Self::Entitlements => "entitlements",
            Self::Profile => "profile",
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The rule an unexpected answer broke. It names fields, never their values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    /// An HTTP status the call doesn't expect, a redirect included.
    Status(u16),
    /// A body over [`MAX_ANSWER_BYTES`].
    BodyTooLarge,
    /// A body that isn't the JSON the call expects.
    NotJson,
    /// A field the call needs is missing.
    Missing(&'static str),
    /// A number outside its range (ADR-0018).
    OutOfRange(&'static str),
    /// A field that isn't the shape the port requires.
    Malformed(&'static str),
    /// The verification link isn't one [`VerificationUri`] accepts.
    LinkNotAllowed,
    /// An OAuth error code the call doesn't map.
    OAuthCode(OAuthCode),
    /// An OAuth error without a readable code.
    UnreadableOAuthError,
    /// An Xbox refusal without a readable `XErr`.
    UnreadableXboxError,
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status(status) => write!(f, "status {status}"),
            Self::BodyTooLarge => write!(f, "body over {} KiB", MAX_ANSWER_BYTES / 1024),
            Self::NotJson => f.write_str("body isn't the expected JSON"),
            Self::Missing(field) => write!(f, "`{field}` is missing"),
            Self::OutOfRange(field) => write!(f, "`{field}` is out of range"),
            Self::Malformed(field) => write!(f, "`{field}` is malformed"),
            Self::LinkNotAllowed => f.write_str("`verification_uri` isn't an allowed link"),
            Self::OAuthCode(code) => write!(f, "OAuth code {code}"),
            Self::UnreadableOAuthError => f.write_str("unreadable OAuth error"),
            Self::UnreadableXboxError => f.write_str("unreadable Xbox error"),
        }
    }
}

/// Why a call to Microsoft failed, whatever the call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    /// No connection: DNS, TCP or a TLS handshake that failed for another
    /// reason than the certificate.
    #[error("Microsoft's servers can't be reached")]
    Unreachable,
    /// The server's certificate was refused. On a Linux server, a missing CA
    /// store causes it too.
    #[error("Microsoft's TLS certificate was refused")]
    Certificate,
    /// No answer within a request's or the call's bound.
    #[error("Microsoft didn't answer in time")]
    TimedOut,
    /// Microsoft answered 429. The caller backs off.
    #[error("Microsoft is rate limiting the server")]
    RateLimited,
    /// Microsoft answered with a server error (5xx).
    #[error("Microsoft answered with server error {status}")]
    Unavailable {
        /// The HTTP status.
        status: u16,
    },
    /// An answer the port can't use.
    #[error("{endpoint}: {rule}")]
    Unexpected {
        /// The request it answered.
        endpoint: Endpoint,
        /// The rule it broke.
        rule: Rule,
    },
}

/// Why a refresh failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RefreshError {
    /// Microsoft refused the refresh token (`invalid_grant`): the account
    /// must be linked again. The only answer that marks it
    /// `reauth_required` (P9.5).
    #[error("Microsoft refused the refresh token: the account must be linked again")]
    InvalidGrant,
    /// Anything else.
    #[error(transparent)]
    Failed(#[from] Failure),
}

/// Why Xbox Live refused the account, from XSTS's `XErr`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XboxRefusal {
    /// 2148916227: banned from Xbox Live.
    Banned,
    /// 2148916233: the Microsoft account has no Xbox profile yet.
    NoXboxProfile,
    /// 2148916235: Xbox Live isn't available in the account's country.
    CountryUnavailable,
    /// 2148916236 or 2148916237: the account needs adult verification.
    AdultVerification,
    /// 2148916238: a child's account that an adult must add to a Microsoft
    /// family.
    ChildAccount,
    /// Another code.
    Other {
        /// Microsoft's code; never secret.
        xerr: u32,
    },
}

impl XboxRefusal {
    /// The reason an account refused this way is stored with, and Xbox
    /// Live's code for [`XboxRefusal::Other`], which goes into the audit
    /// log only (P9.4, P9.5).
    #[must_use]
    pub const fn reason(self) -> (RefusalReason, Option<u32>) {
        match self {
            Self::Banned => (RefusalReason::XboxBanned, None),
            Self::NoXboxProfile => (RefusalReason::XboxNoXboxProfile, None),
            Self::CountryUnavailable => (RefusalReason::XboxCountryUnavailable, None),
            Self::AdultVerification => (RefusalReason::XboxAdultVerification, None),
            Self::ChildAccount => (RefusalReason::XboxChildAccount, None),
            Self::Other { xerr } => (RefusalReason::XboxOther, Some(xerr)),
        }
    }

    /// The refusal `xerr` means.
    #[must_use]
    pub const fn from_xerr(xerr: u32) -> Self {
        match xerr {
            2_148_916_227 => Self::Banned,
            2_148_916_233 => Self::NoXboxProfile,
            2_148_916_235 => Self::CountryUnavailable,
            2_148_916_236 | 2_148_916_237 => Self::AdultVerification,
            2_148_916_238 => Self::ChildAccount,
            xerr => Self::Other { xerr },
        }
    }
}

impl fmt::Display for XboxRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Banned => f.write_str("banned from Xbox Live"),
            Self::NoXboxProfile => f.write_str("the Microsoft account has no Xbox profile"),
            Self::CountryUnavailable => {
                f.write_str("Xbox Live isn't available in the account's country")
            }
            Self::AdultVerification => f.write_str("the account needs adult verification"),
            Self::ChildAccount => {
                f.write_str("a child's account must be added to a Microsoft family")
            }
            Self::Other { xerr } => write!(f, "Xbox error {xerr}"),
        }
    }
}

/// Why a Minecraft session couldn't be started.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    /// Xbox Live refused the account.
    #[error("Xbox Live refused the account: {0}")]
    Xbox(XboxRefusal),
    /// The account has no entitlement at all.
    #[error("the account doesn't own Minecraft")]
    NotOwned,
    /// The account has no Minecraft Java profile (a 404 `NOT_FOUND`).
    #[error("the account has no Minecraft Java profile")]
    NoProfile,
    /// Anything else.
    #[error(transparent)]
    Failed(#[from] Failure),
}

/// A started device-code flow.
#[derive(Debug)]
pub struct DeviceFlow {
    /// The code to poll with.
    pub code: DeviceCode,
    /// The code the user enters.
    pub user_code: UserCode,
    /// Where the user enters it.
    pub verification_uri: VerificationUri,
    /// When the codes expire, from the server's clock read before the
    /// request and Microsoft's `expires_in` (60 s to 1 h).
    pub expires_at: DateTime<Utc>,
    /// How long to wait between polls.
    pub interval: PollInterval,
}

/// What a poll found.
#[derive(Debug)]
pub enum PollOutcome {
    /// The user hasn't answered yet: poll again after the interval.
    Pending,
    /// Poll more slowly: use [`PollInterval::slowed_down`].
    SlowDown,
    /// The user refused.
    Declined,
    /// The codes expired, or Microsoft no longer knows them.
    Expired,
    /// The user approved.
    Authorized(MicrosoftTokens),
}

/// A Microsoft access token and the refresh token to keep.
#[derive(Debug)]
pub struct MicrosoftTokens {
    /// The access token, for [`MicrosoftAuthProvider::minecraft_session`].
    pub access: MsAccessToken,
    /// When it expires, from the server's clock read before the request.
    pub access_expires_at: DateTime<Utc>,
    /// The refresh token to store: the new one, or, when a refresh's answer
    /// has none, the one that was sent (RFC 6749 §6).
    pub refresh: RefreshToken,
}

/// A Minecraft session: its token and the profile it belongs to. The
/// session-token service (P9.5) caches it and hands out clones.
#[derive(Clone)]
pub struct MinecraftSession {
    /// The token an agent joins with.
    pub token: McToken,
    /// When it expires, from the server's clock read before the login.
    pub expires_at: DateTime<Utc>,
    /// The profile's UUID.
    pub uuid: McUuid,
    /// The profile's name.
    pub name: McUsername,
}

/// Only the expiry: the token, the name and the UUID never reach a log.
impl fmt::Debug for MinecraftSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MinecraftSession")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// Microsoft's account flows, as later groups need them told apart
/// (ADR-0018). Every call is bounded and sends each request once: the
/// callers own retries and backoff.
#[async_trait]
pub trait MicrosoftAuthProvider: Send + Sync + fmt::Debug {
    /// Starts a device-code flow.
    ///
    /// # Errors
    /// A [`Failure`].
    async fn start_device_flow(&self) -> Result<DeviceFlow, Failure>;

    /// Polls `code` once. Declined and expired flows are outcomes, not
    /// errors.
    ///
    /// # Errors
    /// A [`Failure`].
    async fn poll(&self, code: &DeviceCode) -> Result<PollOutcome, Failure>;

    /// Exchanges `token` for a new access token and the refresh token to
    /// store.
    ///
    /// # Errors
    /// [`RefreshError::InvalidGrant`] when Microsoft refuses the token, or a
    /// [`Failure`].
    async fn refresh(&self, token: &RefreshToken) -> Result<MicrosoftTokens, RefreshError>;

    /// Starts a Minecraft session with `access`: Xbox Live, XSTS, the
    /// Minecraft login, the entitlements and the profile.
    ///
    /// # Errors
    /// [`SessionError::Xbox`], [`SessionError::NotOwned`],
    /// [`SessionError::NoProfile`], or a [`Failure`].
    async fn minecraft_session(
        &self,
        access: &MsAccessToken,
    ) -> Result<MinecraftSession, SessionError>;
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn secret(text: &str) -> SecretString {
        SecretString::from(text.to_owned())
    }

    fn of_len(len: usize) -> String {
        "x".repeat(len)
    }

    // --- Tokens ------------------------------------------------------------

    #[rstest]
    #[case::one_byte(of_len(1))]
    #[case::at_the_limit(of_len(MAX_TOKEN_BYTES))]
    #[case::every_visible_character((0x21_u8..=0x7E).map(char::from).collect::<String>())]
    fn a_token_of_visible_ascii_up_to_the_limit_is_accepted(#[case] text: String) {
        let token = RefreshToken::try_from(secret(&text)).unwrap();

        assert_eq!(token.expose_secret(), text);
    }

    #[rstest]
    #[case::empty(String::new(), TokenError::Length)]
    #[case::over_the_limit(of_len(MAX_TOKEN_BYTES + 1), TokenError::Length)]
    #[case::space("abc def".to_owned(), TokenError::Character)]
    #[case::delete("abc\u{7f}".to_owned(), TokenError::Character)]
    #[case::newline("abc\n".to_owned(), TokenError::Character)]
    #[case::non_ascii("abcé".to_owned(), TokenError::Character)]
    fn a_token_outside_the_rules_is_refused(#[case] text: String, #[case] error: TokenError) {
        assert_eq!(DeviceCode::try_from(secret(&text)).err(), Some(error));
        assert_eq!(RefreshToken::try_from(secret(&text)).err(), Some(error));
        assert_eq!(MsAccessToken::try_from(secret(&text)).err(), Some(error));
        assert_eq!(McToken::try_from(secret(&text)).err(), Some(error));
    }

    #[test]
    fn every_token_debugs_without_its_text() {
        let text = "afkfleet-port-debug-marker";

        assert_eq!(
            format!("{:?}", DeviceCode::try_from(secret(text)).unwrap()),
            "DeviceCode(..)"
        );
        assert_eq!(
            format!("{:?}", RefreshToken::try_from(secret(text)).unwrap()),
            "RefreshToken(..)"
        );
        assert_eq!(
            format!("{:?}", MsAccessToken::try_from(secret(text)).unwrap()),
            "MsAccessToken(..)"
        );
        assert_eq!(
            format!("{:?}", McToken::try_from(secret(text)).unwrap()),
            "McToken(..)"
        );
    }

    #[test]
    fn a_token_error_never_names_the_text() {
        assert_eq!(
            TokenError::Length.to_string(),
            "a token is 1 to 16384 bytes long"
        );
        assert_eq!(
            TokenError::Character.to_string(),
            "a token holds only visible ASCII characters"
        );
    }

    // --- The user code -----------------------------------------------------

    #[rstest]
    #[case::shortest("ABCD")]
    #[case::longest("ABCDEFGH-1234567")]
    #[case::microsofts_shape("B7X4KDLQ")]
    fn a_user_code_of_the_allowed_shape_is_accepted(#[case] text: &str) {
        assert_eq!(UserCode::try_from(text).unwrap().as_str(), text);
    }

    #[rstest]
    #[case::too_short("ABC")]
    #[case::too_long("ABCDEFGH-12345678")]
    #[case::lowercase("abcd1234")]
    #[case::space("ABCD 1234")]
    #[case::underscore("ABCD_1234")]
    #[case::non_ascii("ABCDÉ123")]
    fn a_user_code_of_another_shape_is_refused(#[case] text: &str) {
        assert_eq!(UserCode::try_from(text), Err(UserCodeError));
    }

    #[test]
    fn a_user_code_debugs_without_its_text() {
        let code = UserCode::try_from("B7X4KDLQ").unwrap();

        assert_eq!(format!("{code:?}"), "UserCode(..)");
        assert_eq!(
            UserCodeError.to_string(),
            "a user code is 4 to 16 of A-Z, 0-9 and -"
        );
    }

    // --- The verification link ---------------------------------------------

    #[rstest]
    #[case::www("https://www.microsoft.com/link", "https://www.microsoft.com/link")]
    #[case::bare_host("https://microsoft.com/link", "https://microsoft.com/link")]
    #[case::upper_case_host("https://WWW.MICROSOFT.COM/link", "https://www.microsoft.com/link")]
    #[case::explicit_default_port(
        "https://www.microsoft.com:443/link",
        "https://www.microsoft.com/link"
    )]
    #[case::root("https://www.microsoft.com", "https://www.microsoft.com/")]
    fn an_https_link_on_an_allowed_host_is_accepted(#[case] text: &str, #[case] kept: &str) {
        assert_eq!(VerificationUri::try_from(text).unwrap().as_str(), kept);
    }

    #[rstest]
    #[case::http("http://www.microsoft.com/link")]
    #[case::other_host("https://login.live.com/link")]
    #[case::lookalike_host("https://www.microsoft.com.example.com/link")]
    #[case::subdomain("https://evil.microsoft.com/link")]
    #[case::user("https://user@www.microsoft.com/link")]
    #[case::user_and_password("https://user:pw@www.microsoft.com/link")]
    #[case::other_port("https://www.microsoft.com:8443/link")]
    #[case::query("https://www.microsoft.com/link?otc=ABCD1234")]
    #[case::empty_query("https://www.microsoft.com/link?")]
    #[case::fragment("https://www.microsoft.com/link#code")]
    #[case::ip("https://192.0.2.10/link")]
    #[case::not_a_url("www.microsoft.com/link")]
    #[case::empty("")]
    fn any_other_link_is_refused(#[case] text: &str) {
        assert_eq!(VerificationUri::try_from(text), Err(VerificationUriError));
    }

    #[test]
    fn the_link_error_names_the_rule() {
        assert_eq!(
            VerificationUriError.to_string(),
            "a verification link is https to www.microsoft.com or microsoft.com, without a user, another port, a query or a fragment, at most 256 characters"
        );
    }

    /// An allowed link of exactly `len` characters.
    fn link_of(len: usize) -> String {
        let base = "https://www.microsoft.com/";
        format!("{base}{}", "a".repeat(len - base.len()))
    }

    #[test]
    fn a_link_of_256_characters_is_accepted() {
        let link = link_of(VerificationUri::MAX_LEN);

        assert_eq!(VerificationUri::MAX_LEN, 256);
        assert_eq!(
            VerificationUri::try_from(link.as_str()).unwrap().as_str(),
            link
        );
    }

    #[test]
    fn a_longer_link_is_refused() {
        let link = link_of(VerificationUri::MAX_LEN + 1);

        assert_eq!(
            VerificationUri::try_from(link.as_str()),
            Err(VerificationUriError)
        );
    }

    #[test]
    fn the_length_is_the_written_back_links() {
        // The text sent is 4 characters longer (`:443`), the link kept is
        // exactly the longest.
        let host = "https://www.microsoft.com";
        let path = format!("/{}", "a".repeat(VerificationUri::MAX_LEN - host.len() - 1));

        assert_eq!(
            VerificationUri::try_from(format!("{host}:443{path}").as_str())
                .unwrap()
                .as_str()
                .len(),
            VerificationUri::MAX_LEN
        );
        assert_eq!(
            VerificationUri::try_from(format!("{host}:443{path}a").as_str()),
            Err(VerificationUriError)
        );
    }

    // --- The poll interval -------------------------------------------------

    #[rstest]
    #[case::shortest(1)]
    #[case::microsofts(5)]
    #[case::longest(60)]
    fn an_interval_of_1_to_60_seconds_is_accepted(#[case] secs: u64) {
        assert_eq!(
            PollInterval::from_secs(secs).unwrap().get(),
            Duration::from_secs(secs)
        );
    }

    #[rstest]
    #[case::zero(0)]
    #[case::over(61)]
    #[case::huge(u64::MAX)]
    fn an_interval_outside_1_to_60_seconds_is_refused(#[case] secs: u64) {
        assert_eq!(PollInterval::from_secs(secs), Err(PollIntervalError));
    }

    #[rstest]
    #[case::from_five(5, 10)]
    #[case::near_the_cap(58, 60)]
    #[case::at_the_cap(60, 60)]
    fn slowing_down_adds_five_seconds_up_to_the_cap(#[case] from: u64, #[case] to: u64) {
        let interval = PollInterval::from_secs(from).unwrap();

        assert_eq!(interval.slowed_down().get(), Duration::from_secs(to));
    }

    #[test]
    fn the_default_is_five_seconds() {
        assert_eq!(PollInterval::DEFAULT.get(), Duration::from_secs(5));
        assert_eq!(
            PollIntervalError.to_string(),
            "a poll interval is 1 to 60 seconds"
        );
    }

    // --- OAuth codes -------------------------------------------------------

    #[rstest]
    #[case::invalid_request("invalid_request")]
    #[case::one_letter("x")]
    #[case::longest("abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijkl")]
    fn an_oauth_code_of_the_kept_shape_is_kept(#[case] text: &str) {
        let code = OAuthCode::try_from(text).unwrap();

        assert_eq!(code.as_str(), text);
        assert_eq!(code.to_string(), text);
    }

    #[rstest]
    #[case::empty("")]
    #[case::too_long("abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklm")]
    #[case::upper_case("Invalid_request")]
    #[case::digit("error1")]
    #[case::space("invalid request")]
    #[case::hyphen("invalid-request")]
    fn an_oauth_code_of_another_shape_is_refused(#[case] text: &str) {
        assert_eq!(OAuthCode::try_from(text), Err(OAuthCodeError));
    }

    // --- Xbox refusals -----------------------------------------------------

    #[rstest]
    #[case::banned(2_148_916_227, XboxRefusal::Banned)]
    #[case::no_profile(2_148_916_233, XboxRefusal::NoXboxProfile)]
    #[case::country(2_148_916_235, XboxRefusal::CountryUnavailable)]
    #[case::adult_236(2_148_916_236, XboxRefusal::AdultVerification)]
    #[case::adult_237(2_148_916_237, XboxRefusal::AdultVerification)]
    #[case::child(2_148_916_238, XboxRefusal::ChildAccount)]
    #[case::other(2_148_916_262, XboxRefusal::Other { xerr: 2_148_916_262 })]
    #[case::zero(0, XboxRefusal::Other { xerr: 0 })]
    fn xerr_codes_map_to_refusals(#[case] xerr: u32, #[case] refusal: XboxRefusal) {
        assert_eq!(XboxRefusal::from_xerr(xerr), refusal);
    }

    #[rstest]
    #[case::banned(XboxRefusal::Banned, RefusalReason::XboxBanned)]
    #[case::no_profile(XboxRefusal::NoXboxProfile, RefusalReason::XboxNoXboxProfile)]
    #[case::country(XboxRefusal::CountryUnavailable, RefusalReason::XboxCountryUnavailable)]
    #[case::adult(XboxRefusal::AdultVerification, RefusalReason::XboxAdultVerification)]
    #[case::child(XboxRefusal::ChildAccount, RefusalReason::XboxChildAccount)]
    fn every_named_xbox_refusal_has_its_reason_without_a_code(
        #[case] refusal: XboxRefusal,
        #[case] reason: RefusalReason,
    ) {
        assert_eq!(refusal.reason(), (reason, None));
    }

    #[test]
    fn another_xbox_refusal_keeps_its_code() {
        assert_eq!(
            XboxRefusal::Other {
                xerr: 2_148_916_262
            }
            .reason(),
            (RefusalReason::XboxOther, Some(2_148_916_262))
        );
    }

    // --- Texts -------------------------------------------------------------

    #[rstest]
    #[case::device_code(Endpoint::DeviceCode, "device code")]
    #[case::poll(Endpoint::Poll, "poll")]
    #[case::refresh(Endpoint::Refresh, "refresh")]
    #[case::xbox_user(Endpoint::XboxUser, "xbox user")]
    #[case::xsts(Endpoint::Xsts, "xsts")]
    #[case::minecraft_login(Endpoint::MinecraftLogin, "minecraft login")]
    #[case::entitlements(Endpoint::Entitlements, "entitlements")]
    #[case::profile(Endpoint::Profile, "profile")]
    fn each_endpoint_has_its_name(#[case] endpoint: Endpoint, #[case] name: &str) {
        assert_eq!(endpoint.to_string(), name);
    }

    #[rstest]
    #[case::status(Rule::Status(302), "status 302")]
    #[case::body(Rule::BodyTooLarge, "body over 256 KiB")]
    #[case::not_json(Rule::NotJson, "body isn't the expected JSON")]
    #[case::missing(Rule::Missing("refresh_token"), "`refresh_token` is missing")]
    #[case::range(Rule::OutOfRange("expires_in"), "`expires_in` is out of range")]
    #[case::malformed(Rule::Malformed("access_token"), "`access_token` is malformed")]
    #[case::link(Rule::LinkNotAllowed, "`verification_uri` isn't an allowed link")]
    #[case::oauth(
        Rule::OAuthCode(OAuthCode::try_from("invalid_request").unwrap()),
        "OAuth code invalid_request"
    )]
    #[case::unreadable_oauth(Rule::UnreadableOAuthError, "unreadable OAuth error")]
    #[case::unreadable_xbox(Rule::UnreadableXboxError, "unreadable Xbox error")]
    fn each_rule_names_what_it_checks(#[case] rule: Rule, #[case] text: &str) {
        assert_eq!(rule.to_string(), text);
    }

    #[rstest]
    #[case::unreachable(Failure::Unreachable, "Microsoft's servers can't be reached")]
    #[case::certificate(Failure::Certificate, "Microsoft's TLS certificate was refused")]
    #[case::timed_out(Failure::TimedOut, "Microsoft didn't answer in time")]
    #[case::rate_limited(Failure::RateLimited, "Microsoft is rate limiting the server")]
    #[case::unavailable(
        Failure::Unavailable { status: 503 },
        "Microsoft answered with server error 503"
    )]
    #[case::unexpected(
        Failure::Unexpected { endpoint: Endpoint::Profile, rule: Rule::BodyTooLarge },
        "profile: body over 256 KiB"
    )]
    #[case::unexpected_code(
        Failure::Unexpected {
            endpoint: Endpoint::Refresh,
            rule: Rule::OAuthCode(OAuthCode::try_from("invalid_request").unwrap()),
        },
        "refresh: OAuth code invalid_request"
    )]
    fn each_failure_has_its_text(#[case] failure: Failure, #[case] text: &str) {
        assert_eq!(failure.to_string(), text);
    }

    #[rstest]
    #[case::invalid_grant(
        RefreshError::InvalidGrant,
        "Microsoft refused the refresh token: the account must be linked again"
    )]
    #[case::failed(
        RefreshError::Failed(Failure::TimedOut),
        "Microsoft didn't answer in time"
    )]
    fn each_refresh_error_has_its_text(#[case] error: RefreshError, #[case] text: &str) {
        assert_eq!(error.to_string(), text);
    }

    #[rstest]
    #[case::banned(
        XboxRefusal::Banned,
        "Xbox Live refused the account: banned from Xbox Live"
    )]
    #[case::no_profile(
        XboxRefusal::NoXboxProfile,
        "Xbox Live refused the account: the Microsoft account has no Xbox profile"
    )]
    #[case::country(
        XboxRefusal::CountryUnavailable,
        "Xbox Live refused the account: Xbox Live isn't available in the account's country"
    )]
    #[case::adult(
        XboxRefusal::AdultVerification,
        "Xbox Live refused the account: the account needs adult verification"
    )]
    #[case::child(
        XboxRefusal::ChildAccount,
        "Xbox Live refused the account: a child's account must be added to a Microsoft family"
    )]
    #[case::other(
        XboxRefusal::Other { xerr: 2_148_916_262 },
        "Xbox Live refused the account: Xbox error 2148916262"
    )]
    fn each_xbox_refusal_has_its_text(#[case] refusal: XboxRefusal, #[case] text: &str) {
        assert_eq!(SessionError::Xbox(refusal).to_string(), text);
    }

    #[rstest]
    #[case::not_owned(SessionError::NotOwned, "the account doesn't own Minecraft")]
    #[case::no_profile(SessionError::NoProfile, "the account has no Minecraft Java profile")]
    #[case::failed(
        SessionError::Failed(Failure::RateLimited),
        "Microsoft is rate limiting the server"
    )]
    fn each_session_error_has_its_text(#[case] error: SessionError, #[case] text: &str) {
        assert_eq!(error.to_string(), text);
    }

    // --- Debug of the results ----------------------------------------------

    #[test]
    fn a_minecraft_session_debugs_only_its_expiry() {
        let expires_at = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let session = MinecraftSession {
            token: McToken::try_from(secret("afkfleet-port-token-marker")).unwrap(),
            expires_at,
            uuid: "4f1c9a2e-7b3d-4e5f-8a6b-0c1d2e3f4a5b".parse().unwrap(),
            name: "AfkBot1".parse().unwrap(),
        };

        let text = format!("{session:?}");

        assert_eq!(
            text,
            format!("MinecraftSession {{ expires_at: {expires_at:?}, .. }}")
        );
    }

    #[test]
    fn a_device_flow_and_tokens_debug_without_their_secrets() {
        let at = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let flow = DeviceFlow {
            code: DeviceCode::try_from(secret("afkfleet-port-device-marker")).unwrap(),
            user_code: UserCode::try_from("MARKER12").unwrap(),
            verification_uri: VerificationUri::try_from("https://www.microsoft.com/link").unwrap(),
            expires_at: at,
            interval: PollInterval::DEFAULT,
        };
        let tokens = PollOutcome::Authorized(MicrosoftTokens {
            access: MsAccessToken::try_from(secret("afkfleet-port-access-marker")).unwrap(),
            access_expires_at: at,
            refresh: RefreshToken::try_from(secret("afkfleet-port-refresh-marker")).unwrap(),
        });

        let text = format!("{flow:?} {tokens:?}");

        assert!(!text.contains("marker"), "{text}");
        assert!(!text.contains("MARKER12"), "{text}");
        assert!(text.contains("https://www.microsoft.com/link"), "{text}");
    }
}
