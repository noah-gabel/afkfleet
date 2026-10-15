//! The `Debug` and `Drop` of every generated message that holds a secret
//! (ADR-0019).
//!
//! The build script skips prost's derived `Debug` for them, so the derived
//! `Debug` of the oneofs and envelopes that contain them calls these instead,
//! and forgetting one fails to compile. Each `Debug` names the message and the
//! fields that aren't sensitive; the secret and whatever identifies the
//! account stay out, like fleet-server's `MinecraftSession`. Each `Drop` wipes
//! the secret, so a copy that never reaches the conversion (a decode error, a
//! clone, the server's message after it's encoded) doesn't linger in freed
//! memory. The buffers tonic, h2 and rustls read and write can't be wiped; the
//! threat model accepts that.

use core::fmt;

use zeroize::Zeroize;

use crate::generated::OnlineSession;

impl fmt::Debug for OnlineSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Every field is named here, so a new one has to be placed: shown or
        // hidden.
        let Self {
            username: _,
            uuid: _,
            access_token: _,
            expires_at_unix_ms,
        } = self;
        f.debug_struct("OnlineSession")
            .field("expires_at_unix_ms", expires_at_unix_ms)
            .finish_non_exhaustive()
    }
}

impl Drop for OnlineSession {
    fn drop(&mut self) {
        self.access_token.zeroize();
    }
}
