//! A TCP proxy between one agent and the server's gRPC port (ADR-0019,
//! S72). It forwards bytes and closes; TLS runs end to end through it. It
//! dials the port where the server listens at each connection, which a
//! restart moves, and pauses briefly after an accept error rather than
//! spin.
//!
//! - [`Proxy::silence_open`]: the connections open now go silent both ways
//!   and stay open, the client's close included: what the server sees of a
//!   crashed host or a broken route. New connections pass.
//! - [`Proxy::cut_open`]: the connections open now close at once. New ones
//!   pass.
//! - [`Proxy::close_gate`] / [`Proxy::open_gate`]: while the gate is
//!   closed, a new connection is closed as soon as it's accepted, so the
//!   agent's dial fails fast and it backs off.
//!
//! Its tasks die with it.

use core::time::Duration;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinSet;

use super::server::GrpcAddress;

/// How long the proxy waits for the server's port to take a connection.
const DIAL: Duration = Duration::from_secs(5);

/// How long the proxy waits after an accept error before it accepts
/// again, so one that repeats (out of file descriptors) can't spin it, as
/// serve's own port waits (`grpc::listener`'s pace).
const ACCEPT_PAUSE: Duration = Duration::from_millis(100);

/// One agent's proxy; see the [module docs](self).
#[derive(Debug)]
pub(crate) struct Proxy {
    address: SocketAddr,
    shared: Arc<Mutex<Shared>>,
    _accepting: JoinSet<()>,
}

#[derive(Debug, Default)]
struct Shared {
    gate_closed: bool,
    open: Vec<Connection>,
}

/// One forwarded connection's switches.
#[derive(Debug)]
struct Connection {
    silence: watch::Sender<bool>,
    cut: watch::Sender<bool>,
}

impl Shared {
    /// Forgets the connections whose pumps both ended.
    fn prune(&mut self) {
        self.open.retain(|connection| !connection.cut.is_closed());
    }
}

impl Proxy {
    /// A proxy on `127.0.0.1:0` to the server's gRPC port wherever
    /// `upstream` says it is at each connection, its gate open.
    pub(crate) async fn start(upstream: GrpcAddress) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let mut accepting = JoinSet::new();
        accepting.spawn(accept(listener, upstream, Arc::clone(&shared)));
        Self {
            address,
            shared,
            _accepting: accepting,
        }
    }

    /// The agent's `[control_plane] url`: the server's certificate names
    /// `127.0.0.1`.
    pub(crate) fn url(&self) -> String {
        format!("https://127.0.0.1:{}", self.address.port())
    }

    /// New connections are closed at once from now on.
    pub(crate) fn close_gate(&self) {
        self.shared.lock().unwrap().gate_closed = true;
    }

    /// New connections pass again.
    pub(crate) fn open_gate(&self) {
        self.shared.lock().unwrap().gate_closed = false;
    }

    /// The open connections go silent both ways and stay open.
    pub(crate) fn silence_open(&self) {
        let mut shared = self.shared.lock().unwrap();
        shared.prune();
        for connection in &shared.open {
            connection.silence.send_replace(true);
        }
    }

    /// The open connections close.
    pub(crate) fn cut_open(&self) {
        let mut shared = self.shared.lock().unwrap();
        for connection in shared.open.drain(..) {
            connection.cut.send_replace(true);
        }
    }
}

/// Accepts connections until the proxy is dropped, which drops this task's
/// pumps too.
async fn accept(listener: TcpListener, upstream: GrpcAddress, shared: Arc<Mutex<Shared>>) {
    let mut pumps = JoinSet::new();
    loop {
        let Ok((client, _)) = listener.accept().await else {
            tokio::time::sleep(ACCEPT_PAUSE).await;
            continue;
        };
        while pumps.try_join_next().is_some() {}
        if shared.lock().unwrap().gate_closed {
            drop(client);
            continue;
        }
        // A server that's down closes the agent's connection at once.
        let dialled = TcpStream::connect(upstream.get());
        let Ok(Ok(server)) = tokio::time::timeout(DIAL, dialled).await else {
            drop(client);
            continue;
        };
        let (silence, silenced) = watch::channel(false);
        let (cut, cuts) = watch::channel(false);
        let mut held = shared.lock().unwrap();
        held.prune();
        held.open.push(Connection { silence, cut });
        drop(held);
        let (client_read, client_write) = client.into_split();
        let (server_read, server_write) = server.into_split();
        pumps.spawn(pump(
            client_read,
            server_write,
            silenced.clone(),
            cuts.clone(),
        ));
        pumps.spawn(pump(server_read, client_write, silenced, cuts));
    }
}

/// Forwards `from` to `to` until either side closes, the connection is cut,
/// or it's silenced, after which it forwards nothing and holds both halves
/// open until it's cut.
async fn pump(
    mut from: OwnedReadHalf,
    mut to: OwnedWriteHalf,
    mut silenced: watch::Receiver<bool>,
    mut cut: watch::Receiver<bool>,
) {
    let mut bytes = vec![0_u8; 16 * 1024];
    let mut held = cut.clone();
    loop {
        tokio::select! {
            biased;
            // Cancel-safe: waiting for a switch loses nothing.
            () = on(&mut cut) => return,
            () = on(&mut silenced) => {
                on(&mut held).await;
                return;
            }
            // Cancel-safe, as tokio documents `read`.
            read = from.read(&mut bytes) => match read {
                Ok(0) | Err(_) => {
                    let _ = to.shutdown().await;
                    return;
                }
                Ok(read) => {
                    if to.write_all(&bytes[..read]).await.is_err() {
                        return;
                    }
                }
            },
        }
    }
}

/// Waits until `switch` is on, or its sender is gone.
async fn on(switch: &mut watch::Receiver<bool>) {
    // The guard it answers with is dropped here, never held across an
    // await.
    let _ = switch.wait_for(|on| *on).await;
}
