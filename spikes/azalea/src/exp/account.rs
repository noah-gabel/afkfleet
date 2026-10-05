//! P1.8: an azalea account built from an externally supplied Minecraft
//! access token, UUID and name: what an agent gets from the server. The agent
//! never sees a Microsoft token (security rule 6).
//!
//! Commands:
//! - `fetch-token [file]`: device-code login through azalea's re-exported
//!   auth (no cache file); writes name/uuid/token to `file` (default
//!   `secrets/p1.8-account.txt`). **The user runs this, not the AI.**
//! - `account-join [file] [--server addr] [--corrupt-token]`: joins with
//!   [`ExternalTokenAccount`] (default: the local online-mode server
//!   `127.0.0.1:25567`), sends one chat line, reports what happened.
//! - `account-check`: no real credentials. Builds an account with a garbage
//!   token and a placeholder UUID, shows the redacted `Debug`, joins the local
//!   online-mode server, and records how the rejection surfaces. Also joins
//!   with `Account::offline` for comparison.

use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use azalea::{
    Event,
    account::{Account, AccountTrait},
    auth::{
        certs::Certificates,
        sessionserver::{self, ClientSessionServerError, SessionServerJoinOpts},
    },
};
use tokio::runtime;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    Res,
    exp::fail::{describe_event, observe},
    host::Host,
    session::{self, Variant},
};

/// The local online-mode server from `compose.online.yaml`.
const ONLINE_SERVER: &str = "127.0.0.1:25567";
const DEFAULT_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../secrets/p1.8-account.txt"
);

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Shared so the runtime keeps a handle after `Account::from` takes a clone.
#[derive(Clone)]
pub struct ExternalTokenAccount(Arc<Inner>);

struct Inner {
    name: String,
    uuid: Uuid,
    /// Never printed: `Debug` is hand-written below.
    token: Mutex<String>,
    certs: Mutex<Option<Certificates>>,
    /// Kind of the last session-server error. azalea only *logs* a failed
    /// join and raises no event, so this is how the runtime learns about it.
    last_join_error: Mutex<Option<&'static str>>,
    refresh_requests: AtomicU32,
}

impl ExternalTokenAccount {
    pub fn new(name: &str, uuid: Uuid, token: String) -> Self {
        Self(Arc::new(Inner {
            name: name.to_owned(),
            uuid,
            token: Mutex::new(token),
            certs: Mutex::new(None),
            last_join_error: Mutex::new(None),
            refresh_requests: AtomicU32::new(0),
        }))
    }
    pub fn last_join_error(&self) -> Option<&'static str> {
        self.0.last_join_error.lock().ok().and_then(|e| *e)
    }
    pub fn refresh_requests(&self) -> u32 {
        self.0.refresh_requests.load(Ordering::SeqCst)
    }
    pub fn has_certs(&self) -> bool {
        self.0.certs.lock().is_ok_and(|c| c.is_some())
    }
}

impl fmt::Debug for ExternalTokenAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExternalTokenAccount")
            .field("name", &self.0.name)
            .field("uuid", &self.0.uuid)
            .field("token", &"<redacted>")
            .field("has_certs", &self.has_certs())
            .finish()
    }
}

fn error_kind(e: &ClientSessionServerError) -> &'static str {
    match e {
        ClientSessionServerError::HttpError(_) => "HttpError",
        ClientSessionServerError::MultiplayerDisabled => "MultiplayerDisabled",
        ClientSessionServerError::Banned => "Banned",
        ClientSessionServerError::AuthServersUnreachable => "AuthServersUnreachable",
        ClientSessionServerError::InvalidSession => "InvalidSession",
        ClientSessionServerError::Unknown(_) => "Unknown",
        ClientSessionServerError::ForbiddenOperation => "ForbiddenOperation",
        ClientSessionServerError::RateLimited => "RateLimited",
        ClientSessionServerError::UnexpectedResponse { .. } => "UnexpectedResponse",
    }
}

impl AccountTrait for ExternalTokenAccount {
    fn username(&self) -> &str {
        &self.0.name
    }
    fn uuid(&self) -> Uuid {
        self.0.uuid
    }
    fn access_token(&self) -> Option<String> {
        self.0.token.lock().ok().map(|t| t.clone())
    }
    /// azalea calls this once after `InvalidSession`/`ForbiddenOperation` and
    /// then retries `join`. In fleet-mc this is where the agent asks the
    /// control plane for one fresh token (P4.3). Here there is none.
    fn refresh(&self) -> BoxFuture<'_, Result<(), azalea::auth::AuthError>> {
        Box::pin(async move {
            self.0.refresh_requests.fetch_add(1, Ordering::SeqCst);
            warn!(
                "session server rejected the token; refresh requested (no new token in the spike)"
            );
            Ok(())
        })
    }
    fn certs(&self) -> Option<Certificates> {
        self.0.certs.lock().ok().and_then(|c| c.clone())
    }
    fn set_certs(&self, certs: Certificates) {
        if let Ok(mut slot) = self.0.certs.lock() {
            *slot = Some(certs);
        }
    }
    fn join<'a>(
        &'a self,
        public_key: &'a [u8],
        private_key: &'a [u8; 16],
        server_id: &'a str,
        proxy: Option<reqwest::Proxy>,
    ) -> BoxFuture<'a, Result<(), ClientSessionServerError>> {
        Box::pin(async move {
            let token = self.access_token().unwrap_or_default();
            let result = sessionserver::join(SessionServerJoinOpts {
                access_token: &token,
                public_key,
                private_key,
                uuid: &self.0.uuid,
                server_id,
                proxy,
            })
            .await;
            if let Err(e) = &result
                && let Ok(mut slot) = self.0.last_join_error.lock()
            {
                *slot = Some(error_kind(e));
            }
            result
        })
    }
}

/// `name=`, `uuid=`, `token=` lines.
fn read_account_file(path: &str) -> Res<ExternalTokenAccount> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("can't read the account file: {e}"))?;
    let get = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key).map(str::trim).map(str::to_owned))
            .ok_or_else(|| format!("account file has no `{key}` line"))
    };
    let uuid = Uuid::parse_str(&get("uuid=")?).map_err(|_| "bad uuid in the account file")?;
    Ok(ExternalTokenAccount::new(
        &get("name=")?,
        uuid,
        get("token=")?,
    ))
}

pub fn fetch_token(args: &[String]) -> Res {
    let path = args.first().map_or(DEFAULT_FILE, String::as_str).to_owned();
    let rt = runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let client = reqwest::Client::new();
        let code = azalea::auth::get_ms_link_code(&client, None, None).await?;
        info!(
            "Open {} and enter the code {} (expires in {} s)",
            code.verification_uri, code.user_code, code.expires_in
        );
        let msa = azalea::auth::get_ms_auth_token(&client, code, None).await?;
        let mc = azalea::auth::get_minecraft_token(&client, &msa.data.access_token).await?;
        let profile = azalea::auth::get_profile(&client, &mc.minecraft_access_token).await?;
        if let Some(parent) = std::path::Path::new(&path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            &path,
            format!("name={}\nuuid={}\ntoken={}\n", profile.name, profile.id, mc.minecraft_access_token),
        )?;
        info!("Wrote the account file (name, UUID and token are not shown). Delete it when you're done.");
        Ok(())
    })
}

pub fn account_join(args: &[String]) -> Res {
    let mut path = DEFAULT_FILE.to_owned();
    let mut server = ONLINE_SERVER.to_owned();
    let mut corrupt = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--server" => server = it.next().ok_or("--server needs an address")?.clone(),
            "--corrupt-token" => corrupt = true,
            other => path = other.to_owned(),
        }
    }
    let mut account = read_account_file(&path)?;
    if corrupt {
        account = ExternalTokenAccount::new(
            &account.0.name,
            account.0.uuid,
            "corrupted-token".to_owned(),
        );
    }
    info!(?account, corrupt, "account loaded (Debug is redacted)");
    run_join(account, server, true)
}

pub fn account_check(_args: &[String]) -> Res {
    let placeholder = Uuid::from_u128(0x1234_5678_9abc_4def_8123_456789abcdef);
    let account = ExternalTokenAccount::new("AfkBot1", placeholder, "not-a-real-token".to_owned());
    info!(
        ?account,
        "Debug output of an account (token must be redacted)"
    );
    info!("--- ExternalTokenAccount with a garbage token");
    run_join(account, ONLINE_SERVER.to_owned(), false)?;
    info!("--- Account::offline against the online-mode server");
    let rt = runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async {
        let host = Host::spawn("mc-host-0")?;
        let mut s = host
            .run(|| {
                session::connect(
                    Variant::Custom,
                    Account::offline("AfkBot1"),
                    ONLINE_SERVER.to_owned(),
                    None,
                )
            })
            .await??;
        observe(&mut s, 15).await;
        s.client.exit();
        Ok(())
    })
}

fn run_join(account: ExternalTokenAccount, server: String, chat: bool) -> Res {
    let rt = runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let host = Host::spawn("mc-host-0")?;
        let handle = account.clone();
        let mut s = host
            .run(move || session::connect(Variant::Custom, Account::from(account), server, None))
            .await??;
        let mut spawned = false;
        let _ = tokio::time::timeout(Duration::from_secs(45), async {
            while let Some(e) = s.events.recv().await {
                info!(event = %describe_event(&e), "event");
                match e {
                    Event::Spawn => {
                        spawned = true;
                        break;
                    }
                    Event::Disconnect(_) | Event::ConnectionFailed(_) => break,
                    _ => {}
                }
            }
        })
        .await;
        info!(
            spawned,
            last_join_error = ?handle.last_join_error(),
            refresh_requests = handle.refresh_requests(),
            "after joining"
        );
        if spawned && chat {
            tokio::time::sleep(Duration::from_secs(3)).await;
            info!(
                has_certs = handle.has_certs(),
                "chat-signing certificates fetched?"
            );
            let c = s.client.clone();
            host.run(move || async move { c.chat("hello from the afkfleet P1.8 spike") })
                .await?;
            observe(&mut s, 5).await;
        }
        s.client.exit();
        Ok(())
    })
}
