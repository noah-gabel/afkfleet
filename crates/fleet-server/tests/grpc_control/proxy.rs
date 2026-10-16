//! A TCP relay between a test's client and the port that can go silent:
//! from then on it reads both sides and forwards nothing, as a network that
//! drops every packet would. It can also cut every connection at once, as a
//! client that dies would. It counts the connections it accepted and the
//! ones the server closed.

use std::net::SocketAddr;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::support::BOUND;

pub(crate) struct Proxy {
    pub(crate) address: SocketAddr,
    silent: watch::Sender<bool>,
    cut: CancellationToken,
    accepted: watch::Receiver<usize>,
    closed: watch::Receiver<usize>,
    // Dropping it aborts the accept loop, whose own set holds the relays.
    _tasks: JoinSet<()>,
}

impl Proxy {
    /// A relay to `upstream` on `127.0.0.1:0`.
    pub(crate) async fn start(upstream: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (silent, silent_now) = watch::channel(false);
        let (accepted_tx, accepted) = watch::channel(0);
        let (closed_tx, closed) = watch::channel(0);
        let cut = CancellationToken::new();
        let cutting = cut.clone();
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            let mut relays = JoinSet::new();
            while let Ok((client, _)) = listener.accept().await {
                let Ok(server) = TcpStream::connect(upstream).await else {
                    return;
                };
                accepted_tx.send_modify(|count| *count += 1);
                let (client_read, client_write) = client.into_split();
                let (server_read, server_write) = server.into_split();
                relays.spawn(pump(
                    client_read,
                    server_write,
                    silent_now.clone(),
                    cutting.clone(),
                    None,
                ));
                relays.spawn(pump(
                    server_read,
                    client_write,
                    silent_now.clone(),
                    cutting.clone(),
                    Some(closed_tx.clone()),
                ));
            }
        });
        Self {
            address,
            silent,
            cut,
            accepted,
            closed,
            _tasks: tasks,
        }
    }

    /// From now on, nothing passes either way.
    pub(crate) fn go_silent(&self) {
        self.silent.send_replace(true);
    }

    /// Closes every connection at once, both ways.
    pub(crate) fn cut(&self) {
        self.cut.cancel();
    }

    /// How many connections it accepted.
    pub(crate) fn accepted(&self) -> usize {
        *self.accepted.borrow()
    }

    /// How many connections the server closed so far.
    pub(crate) fn closed(&self) -> usize {
        *self.closed.borrow()
    }

    /// Waits until the server closed `count` connections, within `limit`;
    /// whether it did.
    pub(crate) async fn server_closed_within(
        &mut self,
        count: usize,
        limit: core::time::Duration,
    ) -> bool {
        tokio::time::timeout(limit, self.closed.wait_for(|closed| *closed >= count))
            .await
            .is_ok()
    }

    /// Waits within [`BOUND`] until the server closed `count` connections.
    pub(crate) async fn server_closed(&mut self, count: usize) {
        assert!(
            self.server_closed_within(count, BOUND).await,
            "the server kept the connection"
        );
    }
}

/// Copies `from` to `to` until `from` ends or `cut` is cancelled,
/// dropping what it reads while `silent`; counts the end of `from` in
/// `closed` if given.
async fn pump(
    mut from: OwnedReadHalf,
    mut to: OwnedWriteHalf,
    silent: watch::Receiver<bool>,
    cut: CancellationToken,
    closed: Option<watch::Sender<usize>>,
) {
    let mut buffer = vec![0; 64 * 1024];
    loop {
        // Cancel-safe: a read that loses the race read nothing.
        let read = tokio::select! {
            () = cut.cancelled() => return,
            read = from.read(&mut buffer) => read,
        };
        match read {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                if !*silent.borrow() && to.write_all(&buffer[..read]).await.is_err() {
                    break;
                }
            }
        }
    }
    if let Some(closed) = closed {
        closed.send_modify(|count| *count += 1);
    }
}
