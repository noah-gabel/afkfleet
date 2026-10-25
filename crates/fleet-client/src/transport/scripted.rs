//! The scripted [`Transport`] the crate's unit tests run on: each route
//! answers from its script, in order, an answer can be held until the test
//! releases it, and every request is recorded with when it was sent.
//!
//! An upgrade answers from its own script: a switch hands the client one end
//! of an 8-byte in-memory pipe (R27's technique; ADR-0020, R77) whose other
//! end is a [`PipeServer`], the test's side of the WebSocket. Nothing here
//! touches the network, so these tests run on paused time.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt as _, StreamExt as _};
use reqwest::header::{CONNECTION, HeaderMap, HeaderValue, SEC_WEBSOCKET_ACCEPT, UPGRADE};
use secrecy::ExposeSecret as _;
use serde_json::Value;
use tokio::io::{AsyncWriteExt as _, DuplexStream};
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role};
use tokio_tungstenite::tungstenite::{Bytes, Message};

use super::{Incoming, Outgoing, Transport, TransportError, Upgrade, UpgradeRequest};

/// Each way of the in-memory pipes, in bytes: so small that a peer that
/// doesn't read holds the other's writes (R27).
pub(crate) const PIPE_BYTES: usize = 8;

/// An answer the test holds until it releases it.
#[derive(Clone, Default)]
pub(crate) struct Gate {
    pub(crate) arrived: Arc<Notify>,
    pub(crate) release: Arc<Notify>,
}

/// One scripted answer.
pub(crate) struct Reply {
    pub(crate) gate: Option<Gate>,
    pub(crate) result: Result<Incoming, TransportError>,
}

/// What the transport was sent.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Seen {
    pub(crate) route: String,
    pub(crate) bearer: Option<String>,
    pub(crate) body: Option<Value>,
}

/// An upgrade the transport was sent, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeenUpgrade {
    pub(crate) path: String,
    pub(crate) bearer: String,
    /// Its `ticket` query parameter.
    pub(crate) ticket: Option<String>,
    pub(crate) at: Instant,
}

/// How a scripted upgrade answers.
enum UpgradeReply {
    /// A 101 with these headers (the `Sec-WebSocket-Accept` as `accept`
    /// says), handing over this pipe end.
    Switch {
        accept: Accept,
        extra: HeaderMap,
        socket: DuplexStream,
    },
    /// Any other answer.
    Refuse(Incoming),
    /// No answer at all.
    Fail(TransportError),
}

/// The `Sec-WebSocket-Accept` a scripted 101 sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Accept {
    /// The one the request's key derives.
    Right,
    /// This one.
    Wrong(String),
    /// None.
    Missing,
}

/// A transport that answers each route from its script, in order, and
/// records what it was sent. An upgrade nobody scripted never answers, so a
/// test sees only the connects it scripted.
#[derive(Default)]
pub(crate) struct Scripted {
    replies: Mutex<HashMap<String, VecDeque<Reply>>>,
    upgrades: Mutex<VecDeque<UpgradeReply>>,
    seen: Mutex<Vec<Seen>>,
    seen_upgrades: Mutex<Vec<SeenUpgrade>>,
}

impl Scripted {
    pub(crate) fn push(&self, route: &str, reply: Reply) {
        self.replies
            .lock()
            .unwrap()
            .entry(route.to_owned())
            .or_default()
            .push_back(reply);
    }

    pub(crate) fn on(&self, route: &str, result: Result<Incoming, TransportError>) {
        self.push(route, Reply { gate: None, result });
    }

    pub(crate) fn held(&self, route: &str, incoming: Incoming) -> Gate {
        let gate = Gate::default();
        self.push(
            route,
            Reply {
                gate: Some(gate.clone()),
                result: Ok(incoming),
            },
        );
        gate
    }

    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    pub(crate) fn routes(&self) -> Vec<String> {
        self.seen().into_iter().map(|seen| seen.route).collect()
    }

    pub(crate) fn bearers(&self, route: &str) -> Vec<Option<String>> {
        self.seen()
            .into_iter()
            .filter(|seen| seen.route == route)
            .map(|seen| seen.bearer)
            .collect()
    }

    /// The next upgrade switches, with the right accept, and hands over a
    /// pipe whose other end the test gets.
    pub(crate) fn switch(&self) -> PipeServer {
        self.switch_with(Accept::Right, HeaderMap::new())
    }

    /// The next upgrade switches with `accept` and `extra` headers.
    pub(crate) fn switch_with(&self, accept: Accept, extra: HeaderMap) -> PipeServer {
        let (client, server) = tokio::io::duplex(PIPE_BYTES);
        self.upgrades
            .lock()
            .unwrap()
            .push_back(UpgradeReply::Switch {
                accept,
                extra,
                socket: client,
            });
        PipeServer::new(server)
    }

    /// The next upgrade is answered with `incoming`.
    pub(crate) fn refuse_upgrade(&self, incoming: Incoming) {
        self.upgrades
            .lock()
            .unwrap()
            .push_back(UpgradeReply::Refuse(incoming));
    }

    /// The next upgrade gets no answer: `error`.
    pub(crate) fn fail_upgrade(&self, error: TransportError) {
        self.upgrades
            .lock()
            .unwrap()
            .push_back(UpgradeReply::Fail(error));
    }

    /// The upgrades sent, in order.
    pub(crate) fn upgrades(&self) -> Vec<SeenUpgrade> {
        self.seen_upgrades.lock().unwrap().clone()
    }
}

impl Transport for Scripted {
    type Socket = DuplexStream;

    async fn send(&self, request: Outgoing) -> Result<Incoming, TransportError> {
        let route = format!("{} {}", request.method, request.url.path());
        self.seen.lock().unwrap().push(Seen {
            route: route.clone(),
            bearer: request
                .bearer
                .as_ref()
                .map(|token| token.expose_secret().to_owned()),
            body: request
                .body
                .as_ref()
                .map(|body| serde_json::from_slice(body).unwrap()),
        });
        let reply = self
            .replies
            .lock()
            .unwrap()
            .get_mut(&route)
            .and_then(VecDeque::pop_front);
        let reply = reply.unwrap_or_else(|| panic!("no answer scripted for {route}"));
        if let Some(gate) = reply.gate {
            gate.arrived.notify_one();
            gate.release.notified().await;
        }
        reply.result
    }

    async fn upgrade(
        &self,
        request: UpgradeRequest,
    ) -> Result<Upgrade<DuplexStream>, TransportError> {
        let ticket = request
            .url
            .query_pairs()
            .find(|(name, _)| name == "ticket")
            .map(|(_, value)| value.into_owned());
        self.seen_upgrades.lock().unwrap().push(SeenUpgrade {
            path: request.url.path().to_owned(),
            bearer: request.bearer.expose_secret().to_owned(),
            ticket,
            at: Instant::now(),
        });
        let reply = self.upgrades.lock().unwrap().pop_front();
        match reply {
            None => core::future::pending().await,
            Some(UpgradeReply::Refuse(incoming)) => Ok(Upgrade::Refused(incoming)),
            Some(UpgradeReply::Fail(error)) => Err(error),
            Some(UpgradeReply::Switch {
                accept,
                extra,
                socket,
            }) => {
                let mut headers = extra;
                headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
                headers.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
                let accept = match accept {
                    Accept::Right => Some(derive_accept_key(request.key.as_bytes())),
                    Accept::Wrong(text) => Some(text),
                    Accept::Missing => None,
                };
                if let Some(accept) = accept {
                    headers.insert(SEC_WEBSOCKET_ACCEPT, accept.parse().unwrap());
                }
                Ok(Upgrade::Switched { headers, socket })
            }
        }
    }
}

/// The test's side of a switched upgrade: the WebSocket's server end, over
/// the in-memory pipe.
pub(crate) struct PipeServer {
    raw: Option<DuplexStream>,
    socket: Option<WebSocketStream<DuplexStream>>,
}

impl PipeServer {
    fn new(raw: DuplexStream) -> Self {
        Self {
            raw: Some(raw),
            socket: None,
        }
    }

    /// The WebSocket, framed on first use.
    async fn socket(&mut self) -> &mut WebSocketStream<DuplexStream> {
        if let Some(raw) = self.raw.take() {
            self.socket = Some(WebSocketStream::from_raw_socket(raw, Role::Server, None).await);
        }
        self.socket.as_mut().unwrap()
    }

    /// Sends a text frame.
    pub(crate) async fn send_text(&mut self, text: &str) {
        self.socket().await.send(Message::text(text)).await.unwrap();
    }

    /// Sends a binary frame.
    pub(crate) async fn send_binary(&mut self, bytes: &[u8]) {
        self.socket()
            .await
            .send(Message::Binary(Bytes::copy_from_slice(bytes)))
            .await
            .unwrap();
    }

    /// Sends a ping.
    pub(crate) async fn ping(&mut self) {
        self.socket()
            .await
            .send(Message::Ping(Bytes::from_static(b"p")))
            .await
            .unwrap();
    }

    /// Sends a close frame with `code`, or one without a code.
    pub(crate) async fn close(&mut self, code: Option<u16>) {
        let frame = code.map(|code| CloseFrame {
            code: CloseCode::from(code),
            reason: "closing".into(),
        });
        // The client may be gone already; what it does next is the test's.
        let _ = self.socket().await.send(Message::Close(frame)).await;
    }

    /// Writes `bytes` on the pipe as they are, past the WebSocket's framing.
    pub(crate) async fn raw(&mut self, bytes: &[u8]) {
        self.socket()
            .await
            .get_mut()
            .write_all(bytes)
            .await
            .unwrap();
    }

    /// The next frame the client sent, or `None` once its end is gone.
    pub(crate) async fn next(&mut self) -> Option<Message> {
        self.socket().await.next().await.and_then(Result::ok)
    }
}
