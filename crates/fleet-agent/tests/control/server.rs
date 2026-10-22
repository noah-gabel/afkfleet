//! A fake `AgentControl` server (ADR-0019, U19): the generated service with
//! fleet-proto's `BoundedCodec`, TLS that requires a client certificate the
//! test CA signed, and every `Connect` handed to the test after its hello,
//! which the test accepts or refuses and then plays the server's side of.

use core::time::Duration;
use std::collections::HashMap;
use std::net::SocketAddr;

use fleet_core::bot::BotAccount;
use fleet_core::id::{AgentId, BotId};
use fleet_core::mc::SessionCredentials;
use fleet_core::value::McUsername;
use fleet_proto::control::{
    AgentMessage, Assignment, Hello, MAX_AGENT_MESSAGE_BYTES, MAX_SERVER_MESSAGE_BYTES,
    ServerMessage, SessionGrant, Welcome, read_hello,
};
use fleet_proto::generated as wire;
use fleet_proto::generated::agent_control_server::{AgentControl, AgentControlServer};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status, Streaming};

use crate::pki::TestCa;

/// How long a test waits for something the agent should do soon.
pub(crate) const PATIENCE: Duration = Duration::from_secs(15);

/// The server's answers to one stream.
type Answers = ReceiverStream<Result<wire::ServerMessage, Status>>;

/// A `Connect` whose hello was read, waiting for the test's decision.
pub(crate) struct Pending {
    /// The hello.
    pub(crate) hello: Hello,
    /// The client certificates the TLS handshake presented, as DER.
    pub(crate) certificates: Vec<Vec<u8>>,
    incoming: Streaming<wire::AgentMessage>,
    decide: oneshot::Sender<Result<Answers, Status>>,
}

impl Pending {
    /// Opens the stream: the test plays the server.
    pub(crate) fn accept(self) -> Call {
        let (outgoing, answers) = mpsc::channel(64);
        let _ = self.decide.send(Ok(ReceiverStream::new(answers)));
        Call {
            incoming: self.incoming,
            outgoing,
            accounts: HashMap::new(),
        }
    }

    /// Refuses the call with `status`, as the server's gate does.
    pub(crate) fn refuse(self, status: Status) {
        let _ = self.decide.send(Err(status));
    }
}

/// An open stream, the test playing the server: it answers the agent's
/// session requests with offline grants for the bots it gave.
pub(crate) struct Call {
    incoming: Streaming<wire::AgentMessage>,
    outgoing: mpsc::Sender<Result<wire::ServerMessage, Status>>,
    accounts: HashMap<BotId, McUsername>,
}

impl Call {
    /// Sends `message`.
    pub(crate) async fn send(&self, message: ServerMessage) {
        let wire = wire::ServerMessage::try_from(message).unwrap();
        self.send_raw(wire).await;
    }

    /// Sends `message` as it is, as a server this agent doesn't fully
    /// understand might.
    pub(crate) async fn send_raw(&self, message: wire::ServerMessage) {
        self.outgoing.send(Ok(message)).await.unwrap();
    }

    /// Welcomes the agent as agent `agent`, heartbeats every `interval`.
    pub(crate) async fn welcome(&self, agent: AgentId, interval: Duration) {
        self.send(ServerMessage::Welcome(Welcome {
            heartbeat_interval: interval,
            agent_id: agent,
        }))
        .await;
    }

    /// Sends the full reconcile of `assignments`, whose offline bots' session
    /// requests it answers from then on.
    pub(crate) async fn reconcile(&mut self, assignments: Vec<Assignment>) {
        for assignment in &assignments {
            if let BotAccount::Offline(name) = &assignment.spec.account {
                self.accounts.insert(assignment.spec.id, name.clone());
            }
        }
        self.send(ServerMessage::Reconcile(assignments)).await;
    }

    /// The agent's next message but heartbeats and the session requests it
    /// answers, within [`PATIENCE`]; none once the stream ended.
    pub(crate) async fn next(&mut self) -> Option<AgentMessage> {
        loop {
            let read = tokio::time::timeout(PATIENCE, self.incoming.message())
                .await
                .expect("the agent should send something");
            let message = match read {
                Ok(Some(message)) => AgentMessage::try_from(message).unwrap(),
                Ok(None) | Err(_) => return None,
            };
            match message {
                AgentMessage::Heartbeat => {}
                AgentMessage::SessionRequest(request)
                    if self.accounts.contains_key(&request.bot_id) =>
                {
                    let username = self.accounts[&request.bot_id].clone();
                    self.send(ServerMessage::SessionGrant(SessionGrant {
                        id: request.id,
                        bot_id: request.bot_id,
                        credentials: SessionCredentials::Offline { username },
                    }))
                    .await;
                }
                other => return Some(other),
            }
        }
    }

    /// The agent's next message, heartbeats included, within `within`.
    pub(crate) async fn raw_next(&mut self, within: Duration) -> Option<AgentMessage> {
        match tokio::time::timeout(within, self.incoming.message()).await {
            Ok(Ok(Some(message))) => Some(AgentMessage::try_from(message).unwrap()),
            Ok(Ok(None) | Err(_)) | Err(_) => None,
        }
    }

    /// Reads until the agent ends its request stream, within `within`: every
    /// message read meanwhile, heartbeats included; none when it didn't end.
    pub(crate) async fn until_ended(&mut self, within: Duration) -> Option<Vec<AgentMessage>> {
        let deadline = tokio::time::Instant::now() + within;
        let mut read = Vec::new();
        loop {
            match tokio::time::timeout_at(deadline, self.incoming.message()).await {
                Err(_elapsed) => return None,
                Ok(Ok(Some(message))) => read.push(AgentMessage::try_from(message).unwrap()),
                Ok(Ok(None) | Err(_)) => return Some(read),
            }
        }
    }

    /// Reads until `pick` takes a message, answering as [`Call::next`].
    pub(crate) async fn until<T>(&mut self, mut pick: impl FnMut(&AgentMessage) -> Option<T>) -> T {
        loop {
            let message = self.next().await.expect("the stream should stay open");
            if let Some(picked) = pick(&message) {
                return picked;
            }
        }
    }

    /// Ends the stream with `status`.
    pub(crate) async fn fail(self, status: Status) {
        let _ = self.outgoing.send(Err(status)).await;
    }
}

/// The fake server; see the [module docs](self).
pub(crate) struct FakeServer {
    /// Where it listens.
    pub(crate) address: SocketAddr,
    pending: mpsc::Receiver<Pending>,
    stop: CancellationToken,
    task: JoinHandle<()>,
}

/// The service: every `Connect` goes to the test.
struct Service {
    pending: mpsc::Sender<Pending>,
}

#[tonic::async_trait]
impl AgentControl for Service {
    type ConnectStream = Answers;

    async fn connect(
        &self,
        request: Request<Streaming<wire::AgentMessage>>,
    ) -> Result<Response<Answers>, Status> {
        let certificates = request
            .peer_certs()
            .map(|certificates| certificates.iter().map(|der| der.to_vec()).collect())
            .unwrap_or_default();
        let mut incoming = request.into_inner();
        let first = incoming
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("no hello"))?;
        let hello = read_hello(first).map_err(|_| Status::invalid_argument("not a hello"))?;
        let (decide, decided) = oneshot::channel();
        self.pending
            .send(Pending {
                hello,
                certificates,
                incoming,
                decide,
            })
            .await
            .map_err(|_| Status::unavailable("the test is gone"))?;
        match decided.await {
            Ok(Ok(answers)) => Ok(Response::new(answers)),
            Ok(Err(status)) => Err(status),
            Err(_) => Err(Status::unavailable("the test is gone")),
        }
    }
}

impl FakeServer {
    /// A server of the test CA `ca` on `127.0.0.1:0`.
    pub(crate) async fn start(ca: &TestCa) -> Self {
        Self::start_on(ca, TcpListener::bind("127.0.0.1:0").await.unwrap())
    }

    /// A server of the test CA `ca` on `listener`.
    pub(crate) fn start_on(ca: &TestCa, listener: TcpListener) -> Self {
        let address = listener.local_addr().unwrap();
        let (certificate, key) = ca.server_certificate(3);
        let tls = ServerTlsConfig::new()
            .identity(Identity::from_pem(certificate, key))
            .client_ca_root(Certificate::from_pem(&ca.pem));
        let (sender, pending) = mpsc::channel(8);
        let service = AgentControlServer::new(Service { pending: sender })
            .max_decoding_message_size(MAX_AGENT_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_SERVER_MESSAGE_BYTES);
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let task = tokio::spawn(async move {
            let _ = Server::builder()
                .tls_config(tls)
                .unwrap()
                .add_service(service)
                .serve_with_incoming_shutdown(
                    TcpIncoming::from(listener),
                    stopped.cancelled_owned(),
                )
                .await;
        });
        Self {
            address,
            pending,
            stop,
            task,
        }
    }

    /// Its URL, as the agent's config names it.
    pub(crate) fn url(&self) -> String {
        format!("https://127.0.0.1:{}", self.address.port())
    }

    /// The next `Connect`, within [`PATIENCE`].
    pub(crate) async fn pending(&mut self) -> Pending {
        tokio::time::timeout(PATIENCE, self.pending.recv())
            .await
            .expect("the agent should connect")
            .expect("the server runs")
    }

    /// The next `Connect` within `within`, if one comes.
    pub(crate) async fn pending_within(&mut self, within: Duration) -> Option<Pending> {
        tokio::time::timeout(within, self.pending.recv())
            .await
            .ok()
            .flatten()
    }

    /// Stops listening; its port is free again.
    pub(crate) async fn stop(self) {
        self.stop.cancel();
        let _ = self.task.await;
    }
}
