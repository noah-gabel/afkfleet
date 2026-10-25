//! A TCP relay between a client and the in-process server that keeps every
//! request it carries, head and body, so a test knows each ticket, access
//! token and refresh token the client sent (ADR-0020, S56). It can cut every
//! connection it carries, and leave the next upgrade unanswered.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use fleet_client::ServerUrl;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinSet;

/// The relay; dropping it ends every connection it carries.
pub(crate) struct Relay {
    /// The relay's own URL, for the client.
    pub(crate) url: ServerUrl,
    shared: Arc<Shared>,
    cut: watch::Sender<u64>,
    _running: JoinSet<()>,
}

/// What the relay's connections share.
#[derive(Default)]
struct Shared {
    /// Every request carried, its head and body as text.
    requests: Mutex<Vec<String>>,
    /// Whether the next upgrade is dropped unanswered.
    drop_upgrade: AtomicBool,
}

impl Relay {
    /// A relay on 127.0.0.1:0 to `upstream`.
    pub(crate) async fn start(upstream: &ServerUrl) -> Self {
        let upstream: SocketAddr = upstream
            .as_str()
            .strip_prefix("http://")
            .and_then(|rest| rest.strip_suffix('/'))
            .unwrap()
            .parse()
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = ServerUrl::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let shared = Arc::new(Shared::default());
        let (cut, cuts) = watch::channel(0);
        let mut running = JoinSet::new();
        let accepting = Arc::clone(&shared);
        running.spawn(async move {
            // The connections live in this task's own set, so they end with
            // it, when the relay is dropped.
            let mut connections = JoinSet::new();
            // No timeout: the accept waits for the test's client, and the
            // relay's drop ends it.
            while let Ok((client, _)) = listener.accept().await {
                connections.spawn(carry(
                    client,
                    upstream,
                    Arc::clone(&accepting),
                    cuts.clone(),
                ));
            }
        });
        Self {
            url,
            shared,
            cut,
            _running: running,
        }
    }

    /// Every request carried so far, head and body.
    pub(crate) fn requests(&self) -> Vec<String> {
        self.shared.requests.lock().unwrap().clone()
    }

    /// Cuts every connection it carries now, without a close.
    pub(crate) fn cut(&self) {
        self.cut.send_modify(|cuts| *cuts += 1);
    }

    /// Leaves the next upgrade unanswered: its connection ends once its
    /// request is read.
    pub(crate) fn drop_next_upgrade(&self) {
        self.shared.drop_upgrade.store(true, Ordering::SeqCst);
    }
}

/// Carries one connection until either side ends it or the relay cuts it.
async fn carry(
    client: TcpStream,
    upstream: SocketAddr,
    shared: Arc<Shared>,
    mut cuts: watch::Receiver<u64>,
) {
    cuts.borrow_and_update();
    let Ok(server) = TcpStream::connect(upstream).await else {
        return;
    };
    let (from_client, to_client) = client.into_split();
    let (from_server, to_server) = server.into_split();
    // The connection ends with either side, or the cut: each pump only moves
    // bytes, and dropping them drops both connections.
    tokio::select! {
        () = up(from_client, to_server, &shared) => {}
        () = down(from_server, to_client) => {}
        _ = cuts.changed() => {}
    }
}

/// The client's bytes to the server: each request recorded once it's whole,
/// then sent on; after an upgrade, the socket's frames as they come.
async fn up(mut from: OwnedReadHalf, mut to: OwnedWriteHalf, shared: &Shared) {
    let mut pending = Vec::new();
    let mut buffer = [0; 8192];
    let mut upgraded = false;
    loop {
        // No timeout: the client's own bounds end its requests, and the
        // relay's cut or drop ends this.
        let Ok(read) = from.read(&mut buffer).await else {
            return;
        };
        if read == 0 {
            let _ = to.shutdown().await;
            return;
        }
        pending.extend_from_slice(&buffer[..read]);
        if upgraded {
            if to.write_all(&pending).await.is_err() {
                return;
            }
            pending.clear();
            continue;
        }
        while let Some(length) = whole_request(&pending) {
            let request = String::from_utf8_lossy(&pending[..length]).into_owned();
            let upgrade = request
                .lines()
                .any(|line| line.eq_ignore_ascii_case("upgrade: websocket"));
            shared.requests.lock().unwrap().push(request);
            if upgrade && shared.drop_upgrade.swap(false, Ordering::SeqCst) {
                return;
            }
            if to.write_all(&pending[..length]).await.is_err() {
                return;
            }
            pending.drain(..length);
            if upgrade {
                upgraded = true;
                if to.write_all(&pending).await.is_err() {
                    return;
                }
                pending.clear();
                break;
            }
        }
    }
}

/// The server's bytes to the client, as they come.
async fn down(mut from: OwnedReadHalf, mut to: OwnedWriteHalf) {
    // No timeout, as `up`.
    let _ = tokio::io::copy(&mut from, &mut to).await;
    let _ = to.shutdown().await;
}

/// The length of the whole request at the start of `pending`, its head and
/// its `Content-Length` body, once all of it is there.
fn whole_request(pending: &[u8]) -> Option<usize> {
    let head = pending
        .windows(4)
        .position(|window| window == b"\r\n\r\n")?
        + 4;
    let text = String::from_utf8_lossy(&pending[..head]);
    let body: usize = text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap_or(0);
    (pending.len() >= head + body).then_some(head + body)
}
