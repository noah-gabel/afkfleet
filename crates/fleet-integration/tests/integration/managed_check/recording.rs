//! The managed check's Microsoft adapter (ADR-0019, S73, U21): the real
//! `ReqwestMicrosoftAuth`, every call passed through unchanged, and every
//! secret its answers carry kept for the log check: the device and user
//! codes, the Microsoft tokens, the Minecraft token, the profile's name and
//! UUID. They're kept as the check's [`Secrets`], `SecretString`s that
//! never reach `Debug` (it shows a count) and are dropped with the run.

use core::fmt;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fleet_server::ports::msauth::{
    DeviceCode, DeviceFlow, Failure, MicrosoftAuthProvider, MicrosoftTokens, MinecraftSession,
    MsAccessToken, PollOutcome, RefreshError, RefreshToken, SessionError,
};

use super::secrets::{Kind, Secrets};

/// The recording adapter; see the [module docs](self).
pub(crate) struct Recording {
    inner: Arc<dyn MicrosoftAuthProvider>,
    seen: Mutex<Secrets>,
}

impl fmt::Debug for Recording {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Recording")
            .field("seen", &*self.seen.lock().unwrap())
            .finish_non_exhaustive()
    }
}

impl Recording {
    /// Records what `inner` answers.
    pub(crate) fn new(inner: Arc<dyn MicrosoftAuthProvider>) -> Self {
        Self {
            inner,
            seen: Mutex::new(Secrets::default()),
        }
    }

    /// Everything recorded so far, handed over.
    pub(crate) fn take(&self) -> Secrets {
        core::mem::take(&mut *self.seen.lock().unwrap())
    }

    fn saw(&self, kind: Kind, value: &str) {
        self.seen.lock().unwrap().saw(kind, value);
    }

    fn tokens(&self, tokens: &MicrosoftTokens) {
        self.saw(Kind::MicrosoftToken, tokens.access.expose_secret());
        self.saw(Kind::MicrosoftToken, tokens.refresh.expose_secret());
    }
}

#[async_trait]
impl MicrosoftAuthProvider for Recording {
    async fn start_device_flow(&self) -> Result<DeviceFlow, Failure> {
        let flow = self.inner.start_device_flow().await?;
        self.saw(Kind::DeviceCode, flow.code.expose_secret());
        self.saw(Kind::UserCode, flow.user_code.as_str());
        Ok(flow)
    }

    async fn poll(&self, code: &DeviceCode) -> Result<PollOutcome, Failure> {
        let outcome = self.inner.poll(code).await?;
        if let PollOutcome::Authorized(tokens) = &outcome {
            self.tokens(tokens);
        }
        Ok(outcome)
    }

    async fn refresh(&self, token: &RefreshToken) -> Result<MicrosoftTokens, RefreshError> {
        self.saw(Kind::MicrosoftToken, token.expose_secret());
        let tokens = self.inner.refresh(token).await?;
        self.tokens(&tokens);
        Ok(tokens)
    }

    async fn minecraft_session(
        &self,
        access: &MsAccessToken,
    ) -> Result<MinecraftSession, SessionError> {
        let session = self.inner.minecraft_session(access).await?;
        self.saw(Kind::MinecraftToken, session.token.expose_secret());
        self.saw(Kind::Name, session.name.as_str());
        self.saw(Kind::Uuid, &session.uuid.to_string());
        self.saw(Kind::Uuid, &session.uuid.as_uuid().simple().to_string());
        Ok(session)
    }
}
