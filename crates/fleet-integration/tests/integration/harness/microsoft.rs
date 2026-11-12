//! A Microsoft of a case's own (Plan.md P13.5; ADR-0033): the provider
//! port answered here, never over the network and never with a real
//! account.
//!
//! It keeps what the real one keeps and a crash case needs: **refresh
//! tokens rotate.** Each refresh spends the token it's given and answers
//! the next one; a spent token is refused for good (`invalid_grant`). Which
//! token it still takes is a number in a file in the case's folder, so
//! every server process of the case, one after the other, talks to the same
//! Microsoft: one that dies after Microsoft rotated a token and before it
//! stored the new one leaves the next process with a token Microsoft no
//! longer takes, as in production.
//!
//! [`Stall`] makes one refresh never answer, for a crash case's child
//! process to be killed in: before Microsoft sees the request, or after it
//! rotated the token.

use core::time::Duration;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use fleet_core::system::Clock;
use fleet_server::ports::msauth::{
    DeviceCode, DeviceFlow, Failure, McToken, MicrosoftAuthProvider, MicrosoftTokens,
    MinecraftSession, MsAccessToken, PollInterval, PollOutcome, RefreshError, RefreshToken,
    SessionError, UserCode, VerificationUri,
};
use secrecy::SecretString;

/// The file in a case's folder that holds the number of the refresh token
/// Microsoft still takes.
const STATE: &str = "microsoft-refresh-token";

/// The start of every refresh token; its number follows.
const REFRESH: &str = "case-refresh-token-";

/// The player every linked account is: an invented profile.
pub(crate) const PLAYER: &str = "CaseMsBot";
const PLAYER_UUID: &str = "5a5a5a5a-5a5a-4a5a-9a5a-5a5a5a5a5a5a";

/// Where one refresh stops and never answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stall {
    /// Before Microsoft sees the request: the token it was given stays
    /// good.
    BeforeTheRequest,
    /// After Microsoft rotated the token: the answer with the new one never
    /// arrives, and the one that was sent is spent.
    AfterTheRotation,
}

/// The fake; see the [module docs](self).
#[derive(Debug)]
pub(crate) struct FakeMicrosoft {
    state: PathBuf,
    clock: Arc<dyn Clock>,
    /// Where a refresh stalls, and what it calls first: the child's line
    /// for its case.
    stall: Option<(Stall, fn())>,
}

impl FakeMicrosoft {
    /// The Microsoft of the case whose folder is `dir`, on `clock`.
    pub(crate) fn new(dir: &Path, clock: Arc<dyn Clock>, stall: Option<(Stall, fn())>) -> Self {
        Self {
            state: dir.join(STATE),
            clock,
            stall,
        }
    }

    /// The number of the refresh token Microsoft takes now; 0 before any
    /// link.
    fn valid(&self) -> u64 {
        std::fs::read_to_string(&self.state)
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0)
    }

    fn rotate_to(&self, number: u64) {
        std::fs::write(&self.state, number.to_string()).unwrap();
    }

    /// An access token and refresh token number `number`, good for an hour.
    fn tokens(&self, number: u64) -> MicrosoftTokens {
        MicrosoftTokens {
            access: MsAccessToken::try_from(secret(&format!("case-access-token-{number}")))
                .unwrap(),
            access_expires_at: fleet_core::time::add(self.clock.now(), Duration::from_hours(1)),
            refresh: RefreshToken::try_from(secret(&format!("{REFRESH}{number}"))).unwrap(),
        }
    }

    /// Stops here for good if this is where the case stalls a refresh.
    async fn stall_at(&self, here: Stall) {
        if let Some((at, said)) = self.stall
            && at == here
        {
            said();
            core::future::pending::<()>().await;
        }
    }
}

fn secret(text: &str) -> SecretString {
    SecretString::from(text.to_owned())
}

#[async_trait]
impl MicrosoftAuthProvider for FakeMicrosoft {
    async fn start_device_flow(&self) -> Result<DeviceFlow, Failure> {
        Ok(DeviceFlow {
            code: DeviceCode::try_from(secret("case-device-code")).unwrap(),
            user_code: UserCode::try_from("CASE1234").unwrap(),
            verification_uri: VerificationUri::try_from("https://www.microsoft.com/link").unwrap(),
            expires_at: fleet_core::time::add(self.clock.now(), Duration::from_mins(15)),
            interval: PollInterval::from_secs(PollInterval::MIN.as_secs()).unwrap(),
        })
    }

    /// The user approved at once.
    async fn poll(&self, _code: &DeviceCode) -> Result<PollOutcome, Failure> {
        let number = self.valid() + 1;
        self.rotate_to(number);
        Ok(PollOutcome::Authorized(self.tokens(number)))
    }

    async fn refresh(&self, token: &RefreshToken) -> Result<MicrosoftTokens, RefreshError> {
        self.stall_at(Stall::BeforeTheRequest).await;
        let sent = token
            .expose_secret()
            .strip_prefix(REFRESH)
            .and_then(|number| number.parse::<u64>().ok());
        if sent != Some(self.valid()) {
            return Err(RefreshError::InvalidGrant);
        }
        let number = self.valid() + 1;
        self.rotate_to(number);
        self.stall_at(Stall::AfterTheRotation).await;
        Ok(self.tokens(number))
    }

    async fn minecraft_session(
        &self,
        _access: &MsAccessToken,
    ) -> Result<MinecraftSession, SessionError> {
        Ok(MinecraftSession {
            token: McToken::try_from(secret("case-minecraft-token")).unwrap(),
            expires_at: fleet_core::time::add(self.clock.now(), Duration::from_hours(24)),
            uuid: PLAYER_UUID.parse().unwrap(),
            name: PLAYER.parse().unwrap(),
        })
    }
}
