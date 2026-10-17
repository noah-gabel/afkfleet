//! U6 (ADR-0019, R9, S22): an agent's stream lives a virtual hour through
//! the real router, its TLS, its gates and the listener's age, while a
//! connection without a certificate is cut at 80 s.
//!
//! Paused time over tokio's in-memory pipes, which hyper-util's `TokioIo`
//! hands tonic's client. Behind the port is a fake of `AgentConnections`
//! that admits one certificate: sqlx can't run on paused time (R22). The
//! enrollment route needs a store but is never called: its database is
//! opened and closed before time is paused.

use core::pin::Pin;
use core::task::{Context, Poll};
use core::time::Duration;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use fleet_core::id::BotId;
use fleet_proto::control::{RejectReason, ServerMessage};
use fleet_server::grpc::registry::SendError;
use tokio::sync::{Semaphore, watch};

use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use fleet_core::id::AgentId;
use fleet_core::pki::CertFingerprint;
use fleet_core::system::Clock as _;
use fleet_core::value::{AgentCapacity, AgentName, AgentVersion};
use fleet_proto::generated::agent_control_client::AgentControlClient;
use fleet_server::app::enrollment::EnrollmentService;
use fleet_server::config::{Heartbeats, RateLimitConfig};
use fleet_server::grpc::control::{
    ControlPorts, ControlTimings, ONE_HELLO, Sessions, control_service,
};
use fleet_server::grpc::enrollment::enrollment_service;
use fleet_server::grpc::listener::{self, Bounded, CONNECTION_AGE};
use fleet_server::grpc::registry::AgentRegistry;
use fleet_server::http::middleware::RequestIds;
use fleet_server::http::rate_limit::RateLimits;
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::pki::{AgentIssuer, Ed25519Key};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::infra::system::SystemClock;
use fleet_server::ports::agent_connections::{
    AdmitError, Admitted, AgentConnections, Conflict, Ended, LiveCertificate, Presented, StreamEnd,
};
use fleet_server::ports::ingest::NoIngest;
use fleet_server::ports::live_agents::PermissionEnd;
use fleet_server::ports::session_grants::{GrantRequest, GrantStream, SessionGrants};
use fleet_testkit::system::{ManualClock, SeededRandom};
use hyper_util::rt::TokioIo;
use sqlx::sqlite::SqliteConnectOptions;
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity, Uri};

use super::support::{BOUND, Call, heartbeat, hello, identity, pki};

/// Grants nothing: no stream here asks for a session.
#[derive(Debug)]
struct NoGrants;

#[async_trait]
impl SessionGrants for NoGrants {
    async fn grant(&self, _stream: GrantStream, _request: GrantRequest) {}
}

/// The fake: it admits one certificate, accepts every hello and keeps how
/// its streams ended. With a gate, recording an end waits for
/// [`OneAgent::release`], as a slow database would.
#[derive(Debug)]
struct OneAgent {
    agent: AgentId,
    name: AgentName,
    fingerprint: CertFingerprint,
    not_after: DateTime<Utc>,
    ended: watch::Sender<Vec<StreamEnd>>,
    gate: Option<Semaphore>,
}

impl OneAgent {
    fn ended(&self) -> Vec<StreamEnd> {
        self.ended.borrow().clone()
    }

    /// The ends once one is recorded, within [`BOUND`].
    async fn recorded(&self) -> Vec<StreamEnd> {
        let mut ended = self.ended.subscribe();
        tokio::time::timeout(BOUND, ended.wait_for(|ended| !ended.is_empty()))
            .await
            .unwrap()
            .unwrap()
            .clone()
    }

    /// Lets one gated end be recorded.
    fn release(&self) {
        if let Some(gate) = &self.gate {
            gate.add_permits(1);
        }
    }
}

#[async_trait]
impl AgentConnections for OneAgent {
    async fn admit(&self, presented: Presented) -> Result<Admitted, AdmitError> {
        if presented.fingerprint != self.fingerprint {
            return Err(AdmitError::Refused);
        }
        Ok(Admitted {
            request: presented.request,
            agent: self.agent,
            name: self.name.clone(),
            fingerprint: self.fingerprint,
            not_after: self.not_after,
            peer: presented.peer,
        })
    }

    async fn hello(
        &self,
        _admitted: &Admitted,
        _capacity: AgentCapacity,
        _version: &AgentVersion,
        _running: &[fleet_core::bot::BotSnapshot],
    ) -> Result<(), AdmitError> {
        Ok(())
    }

    async fn name_refused(&self, _admitted: &Admitted) {}

    async fn ended(&self, _admitted: &Admitted, ended: Ended) {
        if let Some(gate) = &self.gate {
            drop(gate.acquire().await.unwrap());
        }
        self.ended.send_modify(|all| all.push(ended.reason));
    }

    async fn conflict(&self, _admitted: &Admitted, _conflict: Conflict) {}

    async fn sweep(
        &self,
        _live: &[LiveCertificate],
    ) -> Result<Vec<(AgentId, PermissionEnd)>, AdmitError> {
        Ok(Vec::new())
    }
}

/// The client's end of a pipe, noting when the server's end went away.
struct Watched {
    io: DuplexStream,
    gone: Arc<AtomicBool>,
}

impl AsyncRead for Watched {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let poll = Pin::new(&mut this.io).poll_read(cx, buf);
        if let Poll::Ready(result) = &poll
            && (result.is_err() || buf.filled().len() == before)
        {
            this.gone.store(true, Ordering::SeqCst);
        }
        poll
    }
}

impl AsyncWrite for Watched {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().io).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
    }
}

/// tonic's client over `io`, trusting `ca_pem`, presenting `identity` if
/// given.
async fn client(
    io: Watched,
    ca_pem: &str,
    identity: Option<Identity>,
) -> AgentControlClient<Channel> {
    let slot = Arc::new(Mutex::new(Some(io)));
    let mut tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(ca_pem))
        .domain_name("localhost");
    if let Some(identity) = identity {
        tls = tls.identity(identity);
    }
    let channel = Endpoint::from_static("https://localhost")
        .tls_config(tls)
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_: Uri| {
            let io = slot.lock().unwrap().take();
            async move {
                io.map(TokioIo::new)
                    .ok_or_else(|| io::Error::other("the pipe was used"))
            }
        }))
        .await
        .unwrap();
    AgentControlClient::new(channel)
}

/// Beats every `interval` from `next` on, until `until`.
async fn beat_until(call: &Call, next: &mut Instant, interval: Duration, until: Instant) {
    while *next <= until {
        sleep_until(*next).await;
        call.send(heartbeat()).await;
        *next += interval;
    }
    sleep_until(until).await;
}

/// The real router over in-memory pipes, its database closed and time
/// paused.
struct Port {
    connections: Arc<OneAgent>,
    registry: AgentRegistry,
    accept: mpsc::Sender<io::Result<Bounded<DuplexStream>>>,
    ca_pem: String,
    identity: Identity,
    sessions: Sessions,
    stop: CancellationToken,
    server: JoinSet<()>,
    _folder: tempfile::TempDir,
}

impl Port {
    async fn start() -> Self {
        Self::start_with(None).await
    }

    /// A port whose fake records an end only once released.
    async fn start_gated() -> Self {
        Self::start_with(Some(Semaphore::new(0))).await
    }

    async fn start_with(gate: Option<Semaphore>) -> Self {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        // sqlx runs here only, before time is paused (R22).
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("fleet.db");
        drop(tokio::fs::File::create(&file).await.unwrap());
        let db = Database::connect(
            SqliteConnectOptions::new().filename(&file),
            DatabaseOptions::default(),
        )
        .await
        .unwrap();
        db.close().await;
        tokio::time::pause();
        let clock = ManualClock::new(SystemClock.now());
        let random = SeededRandom::new(5);
        let pki = pki(&random, clock.now());
        let agent = AgentId::new_v7(clock.now(), [1; 10]).unwrap();
        let key = Ed25519Key::generate(&random).unwrap();
        let cert = pki
            .ca
            .issue_agent(&key, agent, TimeDelta::days(60), &random, clock.now())
            .unwrap();
        let connections = Arc::new(OneAgent {
            agent,
            name: AgentName::try_from("agent-1").unwrap(),
            fingerprint: cert.fingerprint,
            not_after: cert.validity.not_after,
            ended: watch::channel(Vec::new()).0,
            gate,
        });
        let limits = RateLimits::new(&RateLimitConfig::default(), Arc::new(clock.clone()));
        let ids = RequestIds::new(Arc::new(clock.clone()), Arc::new(random.clone()));
        let ca_pem = String::from_utf8(pki.ca_cert_pem.clone()).unwrap();
        let tls = listener::tls(&pki);
        let enrollment = enrollment_service(
            EnrollmentService::new(
                Arc::new(clock.clone()),
                Arc::new(random.clone()),
                Arc::new(db),
                Arc::new(Sha256Tokens::new(Arc::new(random.clone()))),
                Arc::new(AgentIssuer::new(
                    pki.ca,
                    Arc::new(random.clone()),
                    TimeDelta::days(60),
                )),
            ),
            limits.clone(),
            ids.clone(),
            fleet_proto::enrollment::SERVER_DEADLINE,
        );
        let sessions = Sessions::new(CancellationToken::new());
        let registry = AgentRegistry::new(Arc::new(clock.clone()));
        let control = control_service(
            ControlPorts {
                connections: connections.clone(),
                grants: Arc::new(NoGrants),
                ingest: Arc::new(NoIngest),
                clock: Arc::new(clock.clone()),
            },
            limits,
            ids,
            ControlTimings::new(Heartbeats::default()),
            sessions.clone(),
            registry.clone(),
        );
        let router = listener::router(tls, enrollment, control, Heartbeats::default()).unwrap();
        let (accept, incoming) = mpsc::channel(2);
        let stop = CancellationToken::new();
        let mut server = JoinSet::new();
        let serving = stop.clone();
        server.spawn(async move {
            listener::serve_incoming(router, ReceiverStream::new(incoming), serving)
                .await
                .unwrap();
        });
        Self {
            connections,
            registry,
            accept,
            ca_pem,
            identity: identity(&cert.pem, &key),
            sessions,
            stop,
            server,
            _folder: folder,
        }
    }

    /// Hands the server a new pipe, accepted now; its client presents
    /// `identity` if given, and the flag notes when the server's end went
    /// away.
    async fn connect(
        &self,
        identity: Option<Identity>,
    ) -> (AgentControlClient<Channel>, Arc<AtomicBool>) {
        let (io, server_io) = tokio::io::duplex(64 * 1024);
        let peer = SocketAddr::from(([192, 0, 2, 10], 40_000));
        self.accept
            .send(Ok(Bounded::new(server_io, Some(peer), CONNECTION_AGE)))
            .await
            .unwrap();
        let gone = Arc::new(AtomicBool::new(false));
        let watched = Watched {
            io,
            gone: Arc::clone(&gone),
        };
        (client(watched, &self.ca_pem, identity).await, gone)
    }

    async fn stop(mut self) {
        self.stop.cancel();
        tokio::time::timeout(BOUND, self.sessions.shutdown())
            .await
            .unwrap();
        while let Some(ended) = tokio::time::timeout(BOUND, self.server.join_next())
            .await
            .unwrap()
        {
            ended.unwrap();
        }
    }
}

#[tokio::test]
async fn an_agent_stream_lives_an_hour_through_the_real_router_while_an_unverified_connection_is_cut_at_80_s()
 {
    let port = Port::start().await;
    let start = Instant::now();
    let (agent_client, agent_gone) = port.connect(Some(port.identity.clone())).await;
    let (_open_client, open_gone) = port.connect(None).await;
    let mut call = Call::open(agent_client).await.unwrap();
    call.send(hello("agent-1")).await;
    let interval = call.welcome().await.heartbeat_interval;
    let mut next = Instant::now() + interval;

    beat_until(&call, &mut next, interval, start + Duration::from_secs(79)).await;
    assert!(!open_gone.load(Ordering::SeqCst), "cut before its age");
    beat_until(&call, &mut next, interval, start + Duration::from_secs(81)).await;
    assert!(open_gone.load(Ordering::SeqCst), "not cut at its age");
    beat_until(&call, &mut next, interval, start + Duration::from_hours(1)).await;

    assert!(
        !agent_gone.load(Ordering::SeqCst),
        "the agent's connection was cut"
    );
    assert_eq!(port.connections.ended(), Vec::new());
    // Only a running session answers a second hello.
    call.send(hello("agent-1")).await;
    let status = call.ended().await;
    assert_eq!(status.message(), ONE_HELLO, "{status:?}");
    assert_eq!(
        port.connections.recorded().await,
        vec![StreamEnd::ProtocolError]
    );
    port.stop().await;
}

#[tokio::test]
async fn an_ended_streams_reject_leaves_before_its_end_is_recorded_and_nothing_more_is_queued() {
    let port = Port::start_gated().await;
    let agent = port.connections.agent;
    let (client, _gone) = port.connect(Some(port.identity.clone())).await;
    let mut call = Call::open(client).await.unwrap();
    call.send(hello("agent-1")).await;
    let _ = call.welcome().await;

    assert!(port.registry.end(agent, StreamEnd::Disabled));
    let queued = port.registry.send(
        agent,
        ServerMessage::Unassign(BotId::new_v7(DateTime::UNIX_EPOCH, [1; 10]).unwrap()),
    );

    assert_eq!(queued, Err(SendError::NotConnected));
    // The fake's record waits: the Reject didn't wait for it.
    assert_eq!(call.rejected().await, RejectReason::AgentDisabled);
    assert_eq!(port.connections.ended(), Vec::new());
    port.connections.release();
    assert_eq!(port.connections.recorded().await, vec![StreamEnd::Disabled]);
    port.stop().await;
}
