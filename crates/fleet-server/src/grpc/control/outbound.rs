//! [`Outbound`]: a `Connect` stream's answers to its agent: what its
//! session queued, and a last message that goes out ahead of anything
//! queued and ends the stream.
//!
//! The last message is a `Reject` (the stream then ends OK) or a status.
//! Without one, the stream ends OK once every sender of the queue is gone
//! and what they queued went out.

use core::future::Future as _;
use core::pin::Pin;
use core::task::{Context, Poll};

use fleet_proto::generated as wire;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Status;

/// The last thing a stream sends: a message (a `Reject`), then OK, or a
/// status.
pub(super) type Last = Result<wire::ServerMessage, Status>;

/// A `Connect` stream's answers: what its session queued, and a last
/// message that goes out ahead of anything queued and ends the stream.
#[derive(Debug)]
pub struct Outbound {
    last: Option<oneshot::Receiver<Last>>,
    queue: ReceiverStream<wire::ServerMessage>,
    done: bool,
}

impl Outbound {
    /// The answers of a session holding `queue`'s sender and `last`'s.
    pub(super) fn new(
        queue: mpsc::Receiver<wire::ServerMessage>,
        last: oneshot::Receiver<Last>,
    ) -> Self {
        Self {
            last: Some(last),
            queue: ReceiverStream::new(queue),
            done: false,
        }
    }
}

impl Stream for Outbound {
    type Item = Last;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Last>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        if let Some(last) = this.last.as_mut() {
            match Pin::new(last).poll(cx) {
                Poll::Ready(Ok(answer)) => {
                    this.last = None;
                    this.done = true;
                    return Poll::Ready(Some(answer));
                }
                // The session ended without a last message: the queue
                // decides.
                Poll::Ready(Err(_)) => this.last = None,
                Poll::Pending => {}
            }
        }
        match Pin::new(&mut this.queue).poll_next(cx) {
            Poll::Ready(Some(message)) => Poll::Ready(Some(Ok(message))),
            Poll::Ready(None) => {
                this.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::Wake;

    use tokio_stream::StreamExt as _;
    use tonic::Code;

    use super::*;

    fn reject() -> wire::ServerMessage {
        wire::ServerMessage {
            msg: Some(wire::server_message::Msg::Reject(wire::Reject {
                reason: wire::RejectReason::AgentDisabled.into(),
            })),
        }
    }

    fn assign(n: u8) -> wire::ServerMessage {
        wire::ServerMessage {
            msg: Some(wire::server_message::Msg::Unassign(wire::UnassignBot {
                bot_id: vec![n],
            })),
        }
    }

    fn outbound() -> (
        mpsc::Sender<wire::ServerMessage>,
        oneshot::Sender<Last>,
        Outbound,
    ) {
        let (queue, queued) = mpsc::channel(8);
        let (last, lasting) = oneshot::channel();
        (queue, last, Outbound::new(queued, lasting))
    }

    /// A waker that notes it was woken.
    struct Flag(AtomicBool);

    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn queued_messages_go_out_in_order() {
        let (queue, _last, mut outbound) = outbound();
        queue.send(assign(1)).await.unwrap();
        queue.send(assign(2)).await.unwrap();

        assert_eq!(outbound.next().await.unwrap().unwrap(), assign(1));
        assert_eq!(outbound.next().await.unwrap().unwrap(), assign(2));
    }

    #[tokio::test]
    async fn the_last_message_goes_out_ahead_of_anything_queued_and_ends_the_stream() {
        let (queue, last, mut outbound) = outbound();
        queue.send(assign(1)).await.unwrap();
        queue.send(assign(2)).await.unwrap();

        last.send(Ok(reject())).unwrap();

        assert_eq!(outbound.next().await.unwrap().unwrap(), reject());
        assert!(outbound.next().await.is_none());
        assert!(outbound.next().await.is_none(), "it stays ended");
    }

    #[tokio::test]
    async fn a_last_status_ends_the_stream_with_it() {
        let (_queue, last, mut outbound) = outbound();

        last.send(Err(Status::unavailable("bye"))).unwrap();

        let status = outbound.next().await.unwrap().unwrap_err();
        assert_eq!(status.code(), Code::Unavailable);
        assert!(outbound.next().await.is_none());
    }

    #[tokio::test]
    async fn without_a_last_message_the_queue_drains_and_the_stream_ends() {
        let (queue, last, mut outbound) = outbound();
        queue.send(assign(1)).await.unwrap();
        drop(last);
        drop(queue);

        assert_eq!(outbound.next().await.unwrap().unwrap(), assign(1));
        assert!(outbound.next().await.is_none());
    }

    #[test]
    fn a_waiting_stream_wakes_for_its_last_message() {
        let (_queue, last, outbound) = outbound();
        let mut outbound = core::pin::pin!(outbound);
        let flag = Arc::new(Flag(AtomicBool::new(false)));
        let waker = Arc::clone(&flag).into();
        let mut cx = Context::from_waker(&waker);
        assert!(outbound.as_mut().poll_next(&mut cx).is_pending());

        last.send(Ok(reject())).unwrap();

        assert!(flag.0.load(Ordering::SeqCst));
        let polled = outbound.as_mut().poll_next(&mut cx);
        assert!(
            matches!(&polled, Poll::Ready(Some(Ok(message))) if *message == reject()),
            "{polled:?}"
        );
    }
}
