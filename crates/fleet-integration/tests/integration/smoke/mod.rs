//! P12.7's smoke tests (Plan.md P12.7; ADR-0022, S19, U5, U18, U19, U21):
//! the production compose project, built from this source's images, on
//! each proxy path (U5), set up as an operator sets it up, then checked from
//! the outside in. `just test-stack` builds both images and runs them; CI's
//! Stack workflow runs them nightly on arm64 and amd64.
//!
//! Each test brings up its own project (`afkfleet-smoke-caddy`,
//! `-container`, `-host`; the stand-in proxy `afkfleet-smoke-edge`), and:
//! - creates the Owner with `user create-owner --password-stdin` and logs in
//!   through fleet-client with the full 2FA flow;
//! - finds the client's real address in the audit log, never the proxy's,
//!   and a forged `X-Forwarded-For` replaced by exactly the address the
//!   proxy saw, or, on the host path, honoured from the fleet network's
//!   gateway (`trusted_proxies`);
//! - scrapes both metrics ports from inside the project's network, and
//!   finds `/metrics` through the proxy reaching neither, nor the edge
//!   network reaching the server's metrics address (U19);
//! - opens the live events socket and asks the share search, then forces
//!   502s with a `ticket` and a `q`, and finds neither, nor any password,
//!   TOTP secret, recovery code or the enrollment bundle, in any log or
//!   output (ADR-0020, S28, S127);
//! - finds `.env`'s settings in the server's environment (U18).
//!
//! The Caddy path also takes a backup, recreates the server and the agent,
//! and finds the database, the backup and the agent's identity intact on
//! their mounts; measures memory and pids under 75 % of the limits through
//! eight logins at once (never CPU, U21); stops the stack with every exit
//! 0; and runs the server's image alone, read-only, under a name of its own,
//! which refuses to start and leaves nothing behind.
//!
//! Every wait is bounded and wakes on an event: compose's `--wait`, a
//! service's next log line, or a socket's own timeout (ADR-0014's form).

mod chaos;
mod checks;
mod ops;
mod stack;

use core::fmt::Write as _;
use core::time::Duration;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::time::{SystemTime, UNIX_EPOCH};

use fleet_api_types::SecretText;
use fleet_api_types::accounts::CreateOfflineAccountRequest;
use fleet_api_types::audit::{AuditEntryResponse, AuditPageQuery};
use fleet_api_types::auth::LoginRequest;
use fleet_client::{Client, ClientConfig, CustomCa, LoginStep, MemoryStore, ServerUrl, Trust};
use fleet_testkit::docker::{Compose, ContainerState, docker};
use totp_rs::Totp;

use self::checks::{capabilities, mounts, series, stats};
use self::stack::{ProxyPath, QUICK, SERVER_CONFIG, SETTING, SLOW, Stack};

/// The Owner's name.
pub(crate) const OWNER: &str = "SmokeOwner";
/// The address a forged `X-Forwarded-For` claims (a documentation address).
const FORGED: &str = "203.0.113.7";
/// The share search's distinctive query and the forced 502s' ticket.
const SEARCH: &str = "smokeq";
const FORCED_TICKET: &str = "afk_wt_smoke-ticket";
const FORCED_Q: &str = "smokeforcedq";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_smoke_with_caddy() {
    let mut stack = Stack::prepare(ProxyPath::Caddy);
    stack.set_up();

    let port = stack.port(None, "caddy", "443");
    let url = format!("https://localhost:{port}");
    let ca = caddy_root(&stack.compose.clone(), "caddy");
    let password = draw_password();
    stack.transcript.forbid(&password);
    create_owner(&mut stack, &password);
    let owner = log_in(&url, Trust::CustomCa(ca.clone()), &password, &mut stack).await;

    check_caddy_holds_net_bind_service_alone(&mut stack, "caddy");
    check_real_address(&mut stack, &owner, Some("caddy")).await;
    check_metrics(&mut stack, Some("caddy"));
    check_secrets_stay_out_of_logs(&mut stack, &owner, Some("caddy")).await;
    check_settings_reach_the_server(&mut stack);
    check_survival(&mut stack, &owner).await;
    check_limits(&mut stack, &url, &ca).await;
    check_the_image_alone_refuses_to_start(&mut stack);
    check_the_stop(&mut stack);

    stack.collect_logs();
    stack.transcript.assert_clean();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_smoke_behind_a_container_proxy() {
    let mut stack = Stack::prepare(ProxyPath::Container);
    stack.set_up();

    let edge = stack.edge.clone();
    let port = stack.port(edge.as_ref(), "proxy", "443");
    let url = format!("https://localhost:{port}");
    let ca = caddy_root(edge.as_ref().unwrap(), "proxy");
    let password = draw_password();
    stack.transcript.forbid(&password);
    create_owner(&mut stack, &password);
    let owner = log_in(&url, Trust::CustomCa(ca), &password, &mut stack).await;

    check_caddy_holds_net_bind_service_alone(&mut stack, "proxy");
    check_real_address(&mut stack, &owner, Some("proxy")).await;
    check_metrics(&mut stack, Some("proxy"));
    check_secrets_stay_out_of_logs(&mut stack, &owner, Some("proxy")).await;
    check_settings_reach_the_server(&mut stack);

    stack.collect_logs();
    stack.transcript.assert_clean();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_smoke_behind_a_host_proxy() {
    let mut stack = Stack::prepare(ProxyPath::Host);
    stack.set_up();

    let port = stack.port(None, "server", "8080");
    let url = format!("http://127.0.0.1:{port}");
    let password = draw_password();
    stack.transcript.forbid(&password);
    create_owner(&mut stack, &password);
    let owner = log_in(&url, Trust::System, &password, &mut stack).await;

    check_host_proxy_address(&mut stack, &owner, port).await;
    check_metrics(&mut stack, None);
    let metrics = raw_request(
        port,
        "GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert!(metrics.starts_with("HTTP/1.1 404"), "{metrics}");
    assert!(
        !metrics.contains("afkfleet_"),
        "the API's port served metrics"
    );
    check_secrets_stay_out_of_logs(&mut stack, &owner, None).await;
    check_settings_reach_the_server(&mut stack);

    stack.collect_logs();
    stack.transcript.assert_clean();
}

/// A password drawn for this run: 24 random bytes as hex.
pub(crate) fn draw_password() -> String {
    let mut bytes = [0_u8; 24];
    getrandom_fill(&mut bytes);
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// Fills `bytes` from the OS, through fleet-server's own source.
fn getrandom_fill(bytes: &mut [u8]) {
    use fleet_core::system::SecureRandom as _;
    fleet_server::infra::system::OsRandom.fill(bytes).unwrap();
}

/// The Owner, created as the runbook says: the password on stdin, never an
/// argument (Plan.md P12.7's note). `exec`, not `run`: the server's fixed
/// address on the fleet network can't be taken twice.
pub(crate) fn create_owner(stack: &mut Stack, password: &str) {
    let finished = stack
        .compose
        .run_with_stdin(
            &[
                "exec",
                "-T",
                "server",
                "/usr/local/bin/afkfleet-server",
                "user",
                "create-owner",
                "--config",
                SERVER_CONFIG,
                "--username",
                OWNER,
                "--password-stdin",
            ],
            format!("{password}\n").as_bytes(),
            QUICK,
        )
        .unwrap();
    stack.transcript.keep(&finished);
    assert!(
        finished.success,
        "create-owner failed:\n{}",
        finished.stderr
    );
}

/// Where Caddy keeps its internal CA's root certificate.
const CADDY_ROOT: &str = "/data/caddy/pki/authorities/local/root.crt";

/// The root certificate of the Caddy in `service` (afkfleet's or the
/// stand-in's), read into memory only, once Caddy has made it.
fn caddy_root(compose: &Compose, service: &str) -> CustomCa {
    let pem = until_answered(compose, service, || {
        let finished = compose
            .run(&["exec", "-T", service, "cat", CADDY_ROOT], QUICK)
            .ok()?;
        finished.success.then_some(finished.stdout)
    });
    CustomCa::from_pem(&pem).unwrap()
}

/// Calls `attempt` until it answers, waking at each of `service`'s log
/// lines (or every 2 s), within [`SLOW`].
pub(crate) fn until_answered<T>(
    compose: &Compose,
    service: &str,
    mut attempt: impl FnMut() -> Option<T>,
) -> T {
    let follower = compose.follow(service).unwrap();
    let started = std::time::Instant::now();
    loop {
        if let Some(found) = attempt() {
            return found;
        }
        assert!(started.elapsed() < SLOW, "{service} never answered");
        follower.wait(Duration::from_secs(2));
    }
}

/// Seconds since the epoch on this machine, which Docker's VM follows.
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// The Owner's first login at `url` with `trust`; see [`Owner::first`].
async fn log_in(url: &str, trust: Trust, password: &str, stack: &mut Stack) -> Client<MemoryStore> {
    Owner::first(url, trust, password, stack).await.client
}

/// A client of `url` over `trust`, with nothing stored.
fn client(url: &str, trust: Trust) -> Client<MemoryStore> {
    Client::new(
        ClientConfig {
            server: ServerUrl::parse(url).unwrap(),
            trust,
        },
        MemoryStore::new(),
    )
    .unwrap()
}

/// The Owner signed in, with what signs them in again: the password and the
/// authenticator.
pub(crate) struct Owner {
    pub(crate) client: Client<MemoryStore>,
    password: String,
    app: Totp,
    /// When the last code sent was made: the next must be a later step.
    last: u64,
}

impl Owner {
    /// The first login at `url` with `trust`: the setup step, the
    /// authenticator's code, the login with the next step's code (the server
    /// takes one step of drift and only a later step than the last used).
    pub(crate) async fn first(url: &str, trust: Trust, password: &str, stack: &mut Stack) -> Self {
        let client = client(url, trust);
        let login = LoginRequest {
            username: SecretText::new(OWNER.to_owned()),
            password: SecretText::new(password.to_owned()),
        };
        let step = client.login(&login).await.unwrap();
        assert!(matches!(step, LoginStep::SetupRequired { .. }), "{step:?}");
        let enrolled = client.mfa_enroll().await.unwrap();
        stack.transcript.forbid(enrolled.secret.expose());
        stack.transcript.forbid(enrolled.otpauth_uri.expose());
        let app = Totp::from_url(enrolled.otpauth_uri.expose()).unwrap();
        let at = now();
        let confirmed = client
            .mfa_confirm(SecretText::new(app.generate(at).to_string()))
            .await
            .unwrap();
        for code in &confirmed.recovery_codes {
            stack.transcript.forbid(code.expose());
        }
        let step = client.login(&login).await.unwrap();
        assert!(matches!(step, LoginStep::MfaRequired { .. }), "{step:?}");
        client
            .login_mfa(SecretText::new(app.generate(at + 30).to_string()))
            .await
            .unwrap();
        Self {
            client,
            password: password.to_owned(),
            app,
            last: at + 30,
        }
    }

    /// A new login at `url` with `trust`, the same password and
    /// authenticator: the Owner's TOTP secret must still open (after a
    /// restore, a key rotation, a new host). The server takes only a step
    /// later than the last used, and one step of drift, so the code is the
    /// step after the last one, once the clock has reached the last one:
    /// until then the wait wakes at each of `stack`'s server's log lines, or
    /// every 2 s, within [`SLOW`], as every smoke wait does.
    pub(crate) async fn again(&mut self, url: &str, trust: Trust, stack: &Stack) {
        let follower = stack.compose.follow("server").unwrap();
        let started = std::time::Instant::now();
        while now() / 30 < self.last / 30 {
            assert!(
                started.elapsed() < SLOW,
                "the authenticator's next step never came"
            );
            follower.wait(Duration::from_secs(2));
        }
        drop(follower);
        let client = client(url, trust);
        let step = client
            .login(&LoginRequest {
                username: SecretText::new(OWNER.to_owned()),
                password: SecretText::new(self.password.clone()),
            })
            .await
            .unwrap();
        assert!(matches!(step, LoginStep::MfaRequired { .. }), "{step:?}");
        let at = now().max(self.last + 30);
        client
            .login_mfa(SecretText::new(self.app.generate(at).to_string()))
            .await
            .unwrap();
        self.last = at;
        self.client = client;
    }
}

/// The newest audit entries, newest first.
async fn audit(owner: &Client<MemoryStore>) -> Vec<AuditEntryResponse> {
    owner
        .audit(&AuditPageQuery {
            before: None,
            limit: Some(50),
            target_kind: None,
            target_id: None,
        })
        .await
        .unwrap()
        .items
}

/// The addresses of `outcome`'s `auth.login` entries, newest first.
async fn login_ips(owner: &Client<MemoryStore>, outcome: &str) -> Vec<String> {
    audit(owner)
        .await
        .into_iter()
        .filter(|entry| entry.action.as_str() == "auth.login" && entry.outcome.as_str() == outcome)
        .filter_map(|entry| entry.ip.map(|ip| ip.as_str().to_owned()))
        .collect()
}

/// Through a proxy in `proxy` (a service of afkfleet's own project, or the
/// stand-in's): the Owner's login was recorded from the client's address as
/// the proxy saw it, never the proxy's own, so `trusted_proxies` names the
/// proxy (Plan.md P12.7's note); and a forged `X-Forwarded-For` sent through
/// it is ignored, since the proxy replaces it.
async fn check_real_address(stack: &mut Stack, owner: &Client<MemoryStore>, proxy: Option<&str>) {
    let proxy = proxy.unwrap();
    let logged_in = login_ips(owner, "success").await;
    assert_ne!(logged_in.len(), 0);
    assert!(
        logged_in.iter().all(|ip| *ip != stack.net.proxy_address()),
        "the server recorded the proxy, not the client: {logged_in:?}"
    );
    // Over IPv4 (`-4`), so the client the proxy sees is exactly 127.0.0.1.
    let forged = proxy_curl(
        stack,
        proxy,
        &[
            "-4",
            "-H",
            &format!("X-Forwarded-For: {FORGED}"),
            "-H",
            "Content-Type: application/json",
            "--data",
            r#"{"username":"NobodySmoke","password":"not-the-password"}"#,
            "https://localhost/api/v1/auth/login",
        ],
    );
    assert!(forged.ends_with("401"), "{forged}");
    // Exactly the client's address as the proxy saw it: neither the forged
    // one nor the proxy's own, nor anything else (PR #97's review).
    let failed = login_ips(owner, "failure").await;
    assert_eq!(
        failed.first().map(String::as_str),
        Some("127.0.0.1"),
        "the forged login wasn't recorded from the client's address: {failed:?}"
    );
}

/// The host path: the test is the host's proxy. Its connections reach the
/// server from the fleet network's gateway, which `trusted_proxies` names,
/// so the address it forwards is recorded.
async fn check_host_proxy_address(stack: &mut Stack, owner: &Client<MemoryStore>, port: u16) {
    let logged_in = login_ips(owner, "success").await;
    assert_eq!(
        logged_in.first(),
        Some(&stack.net.fleet_gateway()),
        "{logged_in:?}"
    );
    let body = r#"{"username":"NobodySmoke","password":"not-the-password"}"#;
    let answer = raw_request(
        port,
        &format!(
            "POST /api/v1/auth/login HTTP/1.1\r\nHost: localhost\r\nX-Forwarded-For: {FORGED}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    );
    assert!(answer.starts_with("HTTP/1.1 401"), "{answer}");
    assert_eq!(
        login_ips(owner, "failure")
            .await
            .first()
            .map(String::as_str),
        Some(FORGED),
        "the host proxy's X-Forwarded-For wasn't trusted"
    );
}

/// `request` sent as it is to `127.0.0.1:port`; the whole answer.
fn raw_request(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(QUICK)).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    answer
}

/// `curl` inside the proxy container `service` (afkfleet's Caddy, or the
/// stand-in): the body, then the status code on the last line.
fn proxy_curl(stack: &mut Stack, service: &str, args: &[&str]) -> String {
    let compose = if service == "proxy" {
        stack.edge.clone().unwrap()
    } else {
        stack.compose.clone()
    };
    // Trusting Caddy's own root, never `-k` (CLAUDE.md, rule 9).
    let mut full = vec![
        "exec",
        "-T",
        service,
        "curl",
        "-s",
        "--cacert",
        CADDY_ROOT,
        "-m",
        "10",
        "-w",
        "\n%{http_code}",
    ];
    full.extend_from_slice(args);
    let finished = compose.run(&full, QUICK).unwrap();
    stack.transcript.keep(&finished);
    finished.stdout.trim().to_owned()
}

/// Both metrics ports from inside the fleet network, through the `probe`
/// service (R17); `/metrics` through the proxy reaching neither; and the
/// proxy's container unable to reach the server's metrics port at all (U19).
fn check_metrics(stack: &mut Stack, proxy: Option<&str>) {
    let server = stack.ok(&["run", "--rm", "probe"], SLOW);
    let agent = stack.ok(&["run", "--rm", "probe", "http://agent:9481/metrics"], SLOW);
    assert!(
        server.contains("afkfleet_refresh_token_reuses_total"),
        "{server}"
    );
    assert!(agent.contains("afkfleet_mc_host_threads"), "{agent}");
    let Some(proxy) = proxy else {
        return;
    };
    for path in ["/metrics", "/api/metrics", "/api/v1/metrics"] {
        let answer = proxy_curl(stack, proxy, &[&format!("https://localhost{path}")]);
        assert!(answer.ends_with("404"), "{path}: {answer}");
        assert!(
            !answer.contains("afkfleet_"),
            "{path} reached a metrics port"
        );
    }
    for address in ["server".to_owned(), stack.net.server_address()] {
        let answer = proxy_curl(stack, proxy, &[&format!("http://{address}:9480/metrics")]);
        assert!(
            !answer.contains("afkfleet_"),
            "the proxy reached {address}:9480"
        );
        assert!(answer.ends_with("000"), "{address}:9480 answered: {answer}");
    }
}

/// The live events socket opened and the share search asked through the
/// path; then, through a proxy, 502s forced with a ticket and a `q` while
/// the server is stopped, which its error log records without either.
async fn check_secrets_stay_out_of_logs(
    stack: &mut Stack,
    owner: &Client<MemoryStore>,
    proxy: Option<&str>,
) {
    let mut live = owner.live_events();
    let first = tokio::time::timeout(QUICK, live.recv()).await;
    assert!(matches!(first, Ok(Ok(_))), "{first:?}");
    drop(live);
    let account = owner
        .create_offline_account(&CreateOfflineAccountRequest {
            name: SecretText::new("SmokeBot".to_owned()),
        })
        .await
        .unwrap();
    owner.grant_candidates(account.id, SEARCH).await.unwrap();
    stack.transcript.forbid(SEARCH);
    let Some(proxy) = proxy else {
        return;
    };
    stack.transcript.forbid(FORCED_TICKET);
    stack.transcript.forbid(FORCED_Q);
    stack.ok(&["stop", "server"], SLOW);
    for path in [
        format!("/api/v1/events?ticket={FORCED_TICKET}"),
        format!(
            "/api/v1/accounts/{}/grant-candidates?q={FORCED_Q}",
            account.id
        ),
    ] {
        let answer = proxy_curl(stack, proxy, &[&format!("https://localhost{path}")]);
        assert!(answer.ends_with("502"), "{answer}");
    }
    stack.ok(
        &[
            "up",
            "--detach",
            "--wait",
            "--wait-timeout",
            "180",
            "server",
        ],
        SLOW,
    );
    let compose = if proxy == "proxy" {
        stack.edge.clone().unwrap()
    } else {
        stack.compose.clone()
    };
    let log = compose
        .run(&["logs", "--no-color", proxy], QUICK)
        .unwrap()
        .stdout;
    assert!(
        log.contains("/api/v1/events"),
        "the 502 wasn't logged at all:\n{log}"
    );
    assert!(
        log.contains("/grant-candidates"),
        "the 502 wasn't logged at all"
    );
    stack.transcript.keep_text(log);
}

/// `.env`'s server setting reached the server's environment (U18).
fn check_settings_reach_the_server(stack: &mut Stack) {
    let id = stack.container("server");
    let env = stack.docker(&[
        "inspect",
        "--format",
        "{{range .Config.Env}}{{println .}}{{end}}",
        &id,
    ]);
    assert!(
        env.stdout.lines().any(|line| line == SETTING),
        "{}",
        env.stdout
    );
}

/// A backup survives the server's and the agent's recreation, with the
/// database, the session and the agent's identity, each on its mount.
async fn check_survival(stack: &mut Stack, owner: &Client<MemoryStore>) {
    let path = stack.ok(
        &[
            "exec",
            "-T",
            "server",
            "/usr/local/bin/afkfleet-server",
            "backup",
            "--config",
            SERVER_CONFIG,
        ],
        QUICK,
    );
    let path = path.trim().to_owned();
    assert!(path.starts_with("/backups/afkfleet-backup-"), "{path}");
    let hash = stack.ok(
        &["run", "--rm", "--entrypoint", "sha256sum", "probe", &path],
        QUICK,
    );
    let owner_mode = stack.ok(
        &[
            "run",
            "--rm",
            "--entrypoint",
            "stat",
            "probe",
            "-c",
            "%u %a",
            &path,
        ],
        QUICK,
    );
    assert_eq!(owner_mode.trim(), "65532 600");
    let before = (stack.container("server"), stack.container("agent"));
    let entries = audit(owner).await.len();

    stack.ok(
        &[
            "up",
            "--detach",
            "--wait",
            "--wait-timeout",
            "180",
            "--force-recreate",
            "--no-deps",
            "server",
            "agent",
        ],
        SLOW,
    );

    let after = (stack.container("server"), stack.container("agent"));
    assert_ne!(after.0, before.0);
    assert_ne!(after.1, before.1);
    assert!(audit(owner).await.len() >= entries, "the database was lost");
    assert_eq!(
        stack.ok(
            &["run", "--rm", "--entrypoint", "sha256sum", "probe", &path],
            QUICK
        ),
        hash
    );
    let render = stack.ok(&["run", "--rm", "probe"], SLOW);
    assert!(
        series(&render, "afkfleet_backup_last_success_timestamp_seconds")
            .is_some_and(|at| at > 0.0),
        "the newest backup's time wasn't read at the start"
    );
    let server_mounts = stack.docker(&[
        "inspect",
        "--format",
        "{{range .Mounts}}{{.Type}}:{{.Destination}} {{end}}",
        &after.0,
    ]);
    let found = mounts(&server_mounts.stdout);
    assert!(
        found.contains(&("volume".to_owned(), "/data".to_owned())),
        "{found:?}"
    );
    assert!(found.iter().any(|(_, at)| at == "/backups"), "{found:?}");
    let agent = stack.docker(&["inspect", &after.1]);
    let state = ContainerState::from_inspect(&agent.stdout).unwrap();
    assert!(state.running);
}

/// Eight logins at once keep every service under 75 % of its memory and
/// pids limits, with no restart and no OOM kill (U21).
async fn check_limits(stack: &mut Stack, url: &str, ca: &CustomCa) {
    let logins: Vec<_> = (0..8)
        .map(|n| {
            let client = Client::new(
                ClientConfig {
                    server: ServerUrl::parse(url).unwrap(),
                    trust: Trust::CustomCa(ca.clone()),
                },
                MemoryStore::new(),
            )
            .unwrap();
            tokio::spawn(async move {
                let _ = client
                    .login(&LoginRequest {
                        username: SecretText::new(format!("Nobody{n}")),
                        password: SecretText::new("not-the-password".to_owned()),
                    })
                    .await;
            })
        })
        .collect();
    // Three samples while the logins run (U21): memory and pids, never CPU,
    // which argon2 is meant to fill.
    for _ in 0..3 {
        for (service, pids_limit) in [("server", 256_u64), ("agent", 512), ("caddy", 128)] {
            let id = stack.container(service);
            let line = stack.docker(&[
                "stats",
                "--no-stream",
                "--format",
                "{{.MemPerc}} {{.PIDs}}",
                &id,
            ]);
            let (memory, pids) = stats(&line.stdout).unwrap();
            assert!(memory < 75.0, "{service} used {memory} % of its memory");
            assert!(pids * 4 < pids_limit * 3, "{service} ran {pids} processes");
        }
    }
    for login in logins {
        login.await.unwrap();
    }
    for service in ["server", "agent", "caddy"] {
        let id = stack.container(service);
        let inspect = stack.docker(&["inspect", &id]);
        let state = ContainerState::from_inspect(&inspect.stdout).unwrap();
        assert_eq!(state.restart_count, 0, "{service} restarted");
        assert!(!state.oom_killed, "{service} was killed for its memory");
    }
}

/// The image-alone check's container, by a name of the test's own, so a run
/// that times out or panics can't leave it behind unnamed (PR #97's review).
const ALONE: &str = "afkfleet-smoke-image-alone";

/// Removes the container `self.0` when dropped, also when a check panics.
struct RemovedOnDrop(&'static str);

impl Drop for RemovedOnDrop {
    fn drop(&mut self) {
        let _ = docker(&stack::repo(), &["rm", "--force", self.0], QUICK);
    }
}

/// The server's image alone, read-only, with its config and nothing else
/// mounted, refuses to start: no volume, no key, no database. Nothing of it
/// is left afterwards.
fn check_the_image_alone_refuses_to_start(stack: &mut Stack) {
    let config = stack.dir().join("prod").join("server.toml");
    let mount = format!("{}:{SERVER_CONFIG}:ro", config.display());
    let _ = stack.docker(&["rm", "--force", ALONE]);
    let _removed = RemovedOnDrop(ALONE);
    let finished = stack.docker(&[
        "run",
        "--rm",
        "--name",
        ALONE,
        "--read-only",
        "-v",
        &mount,
        stack::SERVER_IMAGE,
    ]);
    assert_eq!(finished.code, Some(1), "{}", finished.stderr);
    let left = stack.docker(&[
        "ps",
        "--all",
        "--quiet",
        "--filter",
        &format!("name=^{ALONE}$"),
    ]);
    assert_eq!(left.stdout.trim(), "", "the image-alone container was left");
}

/// `stop` ends every service with exit 0, never Docker's kill (137), and
/// the server's last line says it stopped (Plan.md P12.2's note).
fn check_the_stop(stack: &mut Stack) {
    let ids: Vec<(&str, String)> = ["server", "agent", "caddy"]
        .into_iter()
        .map(|service| (service, stack.container(service)))
        .collect();
    stack.ok(&["stop"], SLOW);
    for (service, id) in ids {
        let inspect = stack.docker(&["inspect", &id]);
        let state = ContainerState::from_inspect(&inspect.stdout).unwrap();
        assert_eq!(
            state.exit_code, 0,
            "{service} ended with {}",
            state.exit_code
        );
    }
    let log = stack.ok(&["logs", "--no-color", "--no-log-prefix", "server"], QUICK);
    let last = log
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    assert!(last.contains("the server stopped"), "{last}");
}

/// The Caddy in `service` holds `NET_BIND_SERVICE` alone (ADR-0022, S21;
/// ADR-0024): the compose file keeps it in the bounding set, and 2.11.9's
/// binary carries it as a file capability without the effective bit (`+p`),
/// so it's permitted and never effective; nothing else, and nothing
/// inherited or ambient. Caddy binds 80 and 443 as any process in a container
/// may.
fn check_caddy_holds_net_bind_service_alone(stack: &mut Stack, service: &str) {
    let compose = if service == "proxy" {
        stack.edge.clone().unwrap()
    } else {
        stack.compose.clone()
    };
    let finished = compose
        .run(&["exec", "-T", service, "cat", "/proc/1/status"], QUICK)
        .unwrap();
    stack.transcript.keep(&finished);
    let sets = capabilities(&finished.stdout);
    for (name, held) in [
        ("CapInh", "0000000000000000"),
        ("CapPrm", "0000000000000400"),
        ("CapEff", "0000000000000000"),
        ("CapBnd", "0000000000000400"),
        ("CapAmb", "0000000000000000"),
    ] {
        assert!(
            sets.contains(&(name.to_owned(), held.to_owned())),
            "{service}: {sets:?}"
        );
    }
}
