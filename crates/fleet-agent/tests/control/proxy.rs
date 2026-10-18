//! A stalling TCP proxy between the agent and the fake server:
//! [`Proxy::go_silent`] black-holes the connections already open, both
//! ways, while their sockets stay up, as a dead server or a partition does;
//! a connection opened later passes again.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

/// The proxy; dropping it closes everything.
pub(crate) struct Proxy {
    /// Where it listens.
    pub(crate) address: SocketAddr,
    silenced: Arc<Mutex<Vec<Arc<AtomicBool>>>>,
    _accepting: JoinSet<()>,
}

impl Proxy {
    /// A proxy on `127.0.0.1:0` to `upstream`.
    pub(crate) async fn start(upstream: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let silenced: Arc<Mutex<Vec<Arc<AtomicBool>>>> = Arc::default();
        let flags = Arc::clone(&silenced);
        let mut accepting = JoinSet::new();
        accepting.spawn(async move {
            let mut copies = JoinSet::new();
            while let Ok((client, _)) = listener.accept().await {
                let Ok(server) = TcpStream::connect(upstream).await else {
                    continue;
                };
                let silent = Arc::new(AtomicBool::new(false));
                flags.lock().unwrap().push(Arc::clone(&silent));
                let (client_read, client_write) = client.into_split();
                let (server_read, server_write) = server.into_split();
                copies.spawn(copy(client_read, server_write, Arc::clone(&silent)));
                copies.spawn(copy(server_read, client_write, silent));
            }
        });
        Self {
            address,
            silenced,
            _accepting: accepting,
        }
    }

    /// The URL of the server behind it, as the agent's config names it.
    pub(crate) fn url(&self) -> String {
        format!("https://127.0.0.1:{}", self.address.port())
    }

    /// Silences every connection open now.
    pub(crate) fn go_silent(&self) {
        for silent in self.silenced.lock().unwrap().iter() {
            silent.store(true, Ordering::SeqCst);
        }
    }
}

/// Copies `from` to `to` until either ends; once `silent`, it reads on and
/// drops what it reads.
async fn copy(mut from: OwnedReadHalf, mut to: OwnedWriteHalf, silent: Arc<AtomicBool>) {
    let mut buffer = vec![0; 16 * 1024];
    loop {
        let Ok(read) = from.read(&mut buffer).await else {
            return;
        };
        if read == 0 {
            return;
        }
        if silent.load(Ordering::SeqCst) {
            continue;
        }
        if to.write_all(&buffer[..read]).await.is_err() {
            return;
        }
    }
}
