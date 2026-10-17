//! What a `Connect` stream asks the app for a session request its registry
//! passed (Plan.md P10.6; ADR-0019, S57, S59, R39): the request's grant, or
//! its denial, worked out and sent to the stream that asked.
//!
//! The gRPC port calls it through this trait, not the service itself, so the
//! paused-time tests of the stream run with a fake behind it; sqlx's worker
//! threads can't run on paused time (R22).

use core::fmt;
use core::num::NonZeroU64;

use async_trait::async_trait;
use fleet_core::id::{AgentId, BotId, RequestId};
use fleet_core::pki::CertFingerprint;

/// The stream a session request came on, as the server admitted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantStream {
    /// The agent, from its certificate.
    pub agent: AgentId,
    /// The stream's generation: the answer goes only to this stream.
    pub generation: u64,
    /// The certificate the stream connected with, checked again for each
    /// grant (S59).
    pub fingerprint: CertFingerprint,
    /// The stream's request ID, for the log.
    pub request: RequestId,
}

/// A session request, as the stream's registry passed it: for a bot the
/// stream held assigned when it arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantRequest {
    /// The agent's own number of the request, which the answer repeats.
    pub number: NonZeroU64,
    /// The bot it asks for; the account comes from the server's assignment
    /// alone (S3).
    pub bot: BotId,
    /// Whether the bot's last session was rejected, so a session minted
    /// before this request mustn't be handed out.
    pub fresh: bool,
}

/// The session grants; see the [module docs](self).
#[async_trait]
pub trait SessionGrants: Send + Sync + fmt::Debug {
    /// Works out `request`'s answer, checking its permission again when the
    /// grant is issued, and sends it to `stream`, and only to it: a grant
    /// while that stream still holds the bot assigned, a denial otherwise.
    /// Nothing is sent once that stream ended. Dropping the future sends
    /// nothing; a session the server was minting for it is still kept for
    /// the next request.
    async fn grant(&self, stream: GrantStream, request: GrantRequest);
}
