//! [`Outbound`]: one stream's `Connect` request body, pulled by tonic when
//! HTTP/2 has room (ADR-0019, S66, R50).

use core::fmt;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::sync::{Arc, Mutex, PoisonError};

use fleet_proto::control::AgentMessage;
use fleet_proto::generated as wire;
use tokio::time::Sleep;
use tokio_stream::Stream;
use tracing::warn;

use super::Inner;
use super::state::Next;

/// The request stream of one generation: it sends what the outbox says,
/// and a heartbeat after one Welcome interval of nothing sent. It ends when
/// its generation is no longer the outbox's stream, or the outbox
/// overflowed.
pub struct Outbound {
    shared: Arc<Mutex<Inner>>,
    generation: u64,
    /// The heartbeat's timer, from the last message sent.
    heartbeat: Option<Pin<Box<Sleep>>>,
}

impl Outbound {
    pub(super) const fn new(shared: Arc<Mutex<Inner>>, generation: u64) -> Self {
        Self {
            shared,
            generation,
            heartbeat: None,
        }
    }
}

impl fmt::Debug for Outbound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Outbound")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl Stream for Outbound {
    type Item = wire::AgentMessage;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            let next = {
                let mut inner = this.shared.lock().unwrap_or_else(PoisonError::into_inner);
                let next = inner.state.next(this.generation);
                match next {
                    // Registered under the lock every push takes, so no
                    // wake is lost.
                    Next::Wait { .. } => inner.waker = Some(cx.waker().clone()),
                    Next::Send(_) if inner.state.waiting()[3] == 0 => inner.chat_warned = false,
                    Next::Send(_) | Next::End => {}
                }
                next
            };
            let message = match next {
                Next::End => return Poll::Ready(None),
                Next::Send(message) => message,
                Next::Wait { heartbeat: None } => return Poll::Pending,
                Next::Wait {
                    heartbeat: Some(interval),
                } => {
                    let timer = this
                        .heartbeat
                        .get_or_insert_with(|| Box::pin(tokio::time::sleep(interval)));
                    if timer.as_mut().poll(cx).is_pending() {
                        return Poll::Pending;
                    }
                    AgentMessage::Heartbeat
                }
            };
            // Anything sent resets the server's deadline, so the heartbeat
            // waits a whole interval again (S23).
            this.heartbeat = None;
            match wire::AgentMessage::try_from(message) {
                Ok(message) => return Poll::Ready(Some(message)),
                Err(error) => {
                    warn!(%error, "a message couldn't be written for the wire and was dropped");
                }
            }
        }
    }
}
