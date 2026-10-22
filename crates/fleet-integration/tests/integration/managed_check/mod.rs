//! Phase 10's managed check (Plan.md, Phase 10's definition of done and its two notes;
//! ADR-0019, S73, the user's choices U21 and U22): in managed mode, a bot
//! with the user's real Microsoft account, started through
//! `POST /bots/{id}/start`, goes Online on a real online-mode Minecraft
//! server in Docker and goes offline through stop, all through fleet-client:
//! a link, `patch_bot` after a step-up, `start_bot`, `stop_bot`.
//!
//! **It needs the user's real Microsoft account, so only the user runs
//! it**, with `just test-managed-account`; the AI never runs it or the
//! `manual` profile.
//!
//! - **In process:** the server through the harness's serve wiring, over
//!   the real clock, the OS's randomness and the real Microsoft adapter
//!   (recorded, [`recording`]), offline accounts off; the agent enrolled
//!   with it for real and run as main.rs runs it, from an `agent.toml`,
//!   with the real azalea connector.
//! - **Phase 9 group D's pattern:** a vault key drawn for the run and kept
//!   in memory; a fresh `afkfleet-managed-check-…` folder for the
//!   database, the PKI and the agent's identity, deleted at every end, pass,
//!   FAIL or panic (only Ctrl+C or the profile's 10-minute kill leave it);
//!   nothing secret on a command line, in an environment variable or in a
//!   log.
//! - **It prints** the link, the code, the deadline, and pass or FAIL per
//!   step. A FAIL's text names a state, a reason or an API code, never a
//!   path, an address, a name, a UUID or a token.
//! - **Every log line** of every level and target is captured and checked
//!   for every secret the run saw ([`secrets`]); a finding names its kind,
//!   the line's level and target.
//!
//! The waits are real (U22): the link's status read every 2 s for at most
//! 4 minutes, the bot read every 2 s for at most 90 s until Online and 60 s
//! until it's offline, the container's start at most 300 s, beside the
//! link's wait. The steps stop at [`STEPS`], and the whole check, teardown
//! included, ends within [`WORST`], inside the `manual` profile's 10
//! minutes.
//!
//! A setup step that fails is a FAIL line with a fixed text, never a panic
//! carrying a command's output, and teardown never panics.

pub(crate) mod minecraft;
mod pins;
mod recording;
mod secrets;

use core::any::Any;
use core::fmt::{Display, Write as _};
use core::time::Duration;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fleet_agent::config::{
    self as agent_config, AgentConfig, AgentMode, ConfigError, ControlPlaneConfig,
};
use fleet_agent::heartbeat::Heartbeat;
use fleet_agent::managed::identity::ManagedIdentity;
use fleet_agent::managed::link::LinkTimings;
use fleet_agent::run::{Exit, Managed, Outcome, RunParts, run};
use fleet_agent::signals::{ShutdownSignals, Signal};
use fleet_api_types::accounts::{AccountStatus, LinkFlowState};
use fleet_api_types::auth::{LoginRequest, StepUpRequest};
use fleet_api_types::bots::{
    BotPatchRequest, BotResponse, BotStateKind, BotStatusKind, UnplacedReason,
};
use fleet_api_types::{Open, PageQuery, SecretText};
use fleet_client::{Client, ClientConfig, ClientError, LoginStep, MemoryStore, ServerUrl, Trust};
use fleet_core::id::BotId;
use fleet_core::system::{Clock as _, SecureRandom as _};
use fleet_mc::AzaleaConnector;
use fleet_server::infra::msauth::ReqwestMicrosoftAuth;
use fleet_server::infra::system::{OsRandom, SystemClock};
use fleet_testkit::log_capture;
use secrecy::zeroize::Zeroizing;
use secrecy::{ExposeSecret as _, SecretBox, SecretString};
use tokio::sync::{Mutex, mpsc};
use tokio::task::{JoinError, JoinSet};
use tokio::time::Instant;
use tokio::time::error::Elapsed;
use totp_rs::Totp;

use self::minecraft::{Minecraft, STARTUP};
use self::recording::Recording;
use self::secrets::{Kind, Secrets};
use crate::harness::agent::enroll_files;
use crate::harness::server::{OWNER, RealSetup, Server};

/// The longest the setup may take: the server with its PKI and its Owner,
/// the enrollment, the agent's start, all local.
const SETUP: Duration = Duration::from_secs(30);
/// The longest wait for the user to approve the sign-in.
const LINK_WAIT: Duration = Duration::from_mins(4);
/// The longest the container's start may take from its spawn: its
/// healthcheck's [`STARTUP`], and the container's creation and its port's
/// lookup. The Owner's login and the link's wait run beside it.
const CONTAINER: Duration = STARTUP.saturating_add(Duration::from_secs(10));
/// The listing, the step-up, the server's patch, the start and the stop
/// together: local requests.
const REQUESTS: Duration = Duration::from_secs(20);
/// The longest wait for the bot to go Online once started.
const ONLINE_WAIT: Duration = Duration::from_secs(90);
/// The longest wait for the bot to be off its agent once stopped.
const OFFLINE_WAIT: Duration = Duration::from_secs(60);
/// How often the link and the bot are read.
const READ_EVERY: Duration = Duration::from_secs(2);
/// A wait's last read: its pause and its answer.
const LAST_READ: Duration = Duration::from_secs(5);
/// The longest wait for the agent to stop: its fleet's shutdown, 10 s
/// (`[runtime] shutdown_timeout`), then the run's own end.
const AGENT_STOP: Duration = Duration::from_secs(20);
/// The longest wait for the server to stop: serve's own deadline, the
/// drain's 15 s (`[http] request_timeout_secs`) and the database's 5 s.
const SERVER_STOP: Duration = Duration::from_secs(25);
/// The longest wait for the container and its volumes to go.
const REMOVE: Duration = Duration::from_secs(15);

/// The steps' budget, past which they're stopped and teardown runs: the
/// setup, the container's start (the link's wait beside it), the requests,
/// and the two waits for the bot. A request that hangs ends there, so the
/// check never outlives [`WORST`].
const STEPS: Duration = SETUP
    .saturating_add(CONTAINER)
    .saturating_add(REQUESTS)
    .saturating_add(ONLINE_WAIT)
    .saturating_add(LAST_READ)
    .saturating_add(OFFLINE_WAIT)
    .saturating_add(LAST_READ);
/// The check's worst case: the steps, then the agent's, the server's and
/// the container's ends. The teardown's wait for a container still
/// starting ends by the spawn's [`CONTAINER`], inside the steps' budget.
const WORST: Duration = STEPS
    .saturating_add(AGENT_STOP)
    .saturating_add(SERVER_STOP)
    .saturating_add(REMOVE);
// The `manual` profile kills a test at 600 s (`.config/nextest.toml`); the
// log check and the folder's deletion take a moment of the 20 s left.
const _: () = assert!(WORST.as_secs() + 20 <= 600);
// The Owner's login and the link's wait fit beside the container's start.
const _: () = assert!(LINK_WAIT.as_secs() + 30 <= CONTAINER.as_secs());

/// The agent's name.
const AGENT: &str = "check-agent";

/// The steps' results, printed as they come.
#[derive(Debug, Default)]
struct Steps {
    failures: Vec<String>,
}

impl Steps {
    fn pass(step: &str) {
        println!("{step}: pass");
    }

    fn fail(&mut self, step: &str, why: impl Display) {
        println!("{step}: FAIL ({why})");
        self.failures.push(format!("{step}: {why}"));
    }

    fn check(&mut self, step: &str, passed: bool, why: &str) {
        if passed {
            Self::pass(step);
        } else {
            self.fail(step, why);
        }
    }

    /// Checks every log line for every secret the run saw.
    fn check_logs(&mut self, secrets: &Secrets) {
        let findings = secrets.findings();
        if findings.is_empty() {
            Self::pass("no secret in any log");
        } else {
            for finding in findings {
                self.fail("no secret in any log", finding);
            }
        }
    }

    /// Fails the test if any step failed.
    fn assert_passed(self) {
        assert!(
            self.failures.is_empty(),
            "steps failed: {:#?}",
            self.failures
        );
    }
}

/// What every end tears down, whichever step it ended at.
#[derive(Default)]
struct Rig {
    server: Mutex<Option<Server>>,
    agent: Mutex<Option<RunningAgent>>,
    minecraft: Mutex<JoinSet<Result<Minecraft, String>>>,
    /// When the container's start must have ended: its spawn's
    /// [`CONTAINER`].
    container_by: std::sync::Mutex<Option<Instant>>,
    ready: Mutex<Option<Minecraft>>,
    recording: std::sync::Mutex<Option<Arc<Recording>>>,
    secrets: std::sync::Mutex<Secrets>,
}

impl Rig {
    fn saw(&self, kind: Kind, value: &str) {
        lock(&self.secrets).saw(kind, value);
    }
}

/// `mutex`'s guard even when a step panicked while holding it, so teardown
/// and the log check go on with what it holds.
fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The agent's run.
struct RunningAgent {
    signals: mpsc::Sender<Signal>,
    task: JoinSet<Outcome>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_managed_bot_goes_online_and_offline() {
    // Another test in this process may have installed it already.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    log_capture::install().unwrap();
    println!(
        "The check takes at most {} min {} s.",
        WORST.as_secs() / 60,
        WORST.as_secs() % 60
    );
    let dir = tempfile::Builder::new()
        .prefix("afkfleet-managed-check-")
        .tempdir()
        .unwrap();
    let rig = Arc::new(Rig::default());

    // The steps run in a task of their own, so a panic comes back here as an
    // error and the budget can end them: everything stops and the folder
    // goes on every end.
    let mut checking = tokio::spawn(check(Arc::clone(&rig), dir.path().to_owned()));
    let ended = tokio::time::timeout(STEPS, &mut checking).await;
    if ended.is_err() {
        checking.abort();
        // Every await in the steps yields, so the aborted task ends at its
        // next one; it holds nothing teardown needs after that.
        let _ = checking.await;
    }
    let mut teardown = Steps::default();
    tear_down(&rig, dir.path(), &mut teardown).await;
    let secrets = core::mem::take(&mut *lock(&rig.secrets));
    drop(rig);
    let deleted = dir.close().is_ok();
    let (steps, panic) = conclude(ended, teardown, deleted, &secrets);
    if let Some(panic) = panic {
        std::panic::resume_unwind(panic);
    }
    steps.assert_passed();
}

/// What the steps' task ended with: their report, a panic, or nothing within
/// their budget.
type Ended = Result<Result<Steps, JoinError>, Elapsed>;

/// How the check ends once everything's torn down: the steps' report with
/// teardown's, the folder's and the log check's lines, and the steps' panic,
/// if they panicked, to go on with once those are printed.
fn conclude(
    ended: Ended,
    mut teardown: Steps,
    deleted: bool,
    secrets: &Secrets,
) -> (Steps, Option<Box<dyn Any + Send>>) {
    let (mut steps, panic) = match ended {
        Ok(Ok(steps)) => (steps, None),
        Ok(Err(failed)) => {
            if let Ok(panic) = failed.try_into_panic() {
                (Steps::default(), Some(panic))
            } else {
                let mut steps = Steps::default();
                steps.fail("the steps ended", "their task was cancelled");
                (steps, None)
            }
        }
        Err(_) => {
            let mut steps = Steps::default();
            steps.fail(
                "the steps within their budget",
                format!("they didn't end within {} s", STEPS.as_secs()),
            );
            (steps, None)
        }
    };
    steps.failures.append(&mut teardown.failures);
    steps.check(
        "temporary folder deleted",
        deleted,
        "it's still there: delete the afkfleet-managed-check folder in the temp directory",
    );
    steps.check_logs(secrets);
    (steps, panic)
}

/// The check's steps; the first that fails ends them.
async fn check(rig: Arc<Rig>, dir: PathBuf) -> Steps {
    let mut steps = Steps::default();
    let Some(password) = random_password() else {
        steps.fail("server and agent started", "the OS's randomness failed");
        return steps;
    };
    rig.saw(Kind::Password, password.expose_secret());
    let url = match tokio::time::timeout(SETUP, set_up(&rig, &dir, password.clone())).await {
        Ok(Ok(url)) => url,
        Ok(Err(why)) => {
            steps.fail("server and agent started", why);
            return steps;
        }
        Err(_) => {
            steps.fail(
                "server and agent started",
                format!("not within {} s", SETUP.as_secs()),
            );
            return steps;
        }
    };
    Steps::pass("server and agent started (enrolled)");

    // The container starts while the Owner logs in and the link waits.
    *lock(&rig.container_by) = Some(Instant::now() + CONTAINER);
    rig.minecraft.lock().await.spawn(Minecraft::start());

    let (client, store) = match log_in(&rig, &url, &password).await {
        Ok(logged_in) => logged_in,
        Err(why) => {
            steps.fail("Owner logged in with 2FA", why);
            return steps;
        }
    };
    // The client's refresh token, which no log may hold either; every
    // other token of the server's is found by its prefix ([`secrets`]).
    if let Ok(Some(token)) = fleet_client::SessionStore::load(&store).await {
        rig.saw(Kind::RefreshToken, token.expose_secret());
    }
    Steps::pass("Owner logged in with 2FA");

    let Some(account) = link(&rig, &client, &mut steps).await else {
        return steps;
    };
    let Some(bot) = listed(&client, account, &mut steps).await else {
        return steps;
    };
    let Some(address) = minecraft_address(&rig, &mut steps).await else {
        return steps;
    };

    if let Err(why) = configure(&client, &password, bot, &address).await {
        steps.fail("server set after a step-up", why);
        return steps;
    }
    Steps::pass("server set after a step-up");
    if let Err(error) = client.start_bot(bot).await {
        steps.fail("started", describe(&error));
        return steps;
    }
    Steps::pass("started");
    if !online(&client, bot, &mut steps).await {
        return steps;
    }
    if let Err(error) = client.stop_bot(bot).await {
        steps.fail("stopped", describe(&error));
        return steps;
    }
    Steps::pass("stopped");
    offline(&client, bot, &mut steps).await;
    drop(store);
    steps
}

/// The server, its Microsoft adapter recorded, then the agent enrolled with
/// it and started: each in the rig as soon as it runs, so every end stops
/// it. Returns the API's URL; a failure is a fixed text.
async fn set_up(rig: &Rig, dir: &Path, password: SecretString) -> Result<String, String> {
    let adapter = ReqwestMicrosoftAuth::new(Arc::new(SystemClock))
        .map_err(|_| "Microsoft's HTTP client can't be built")?;
    let recording = Arc::new(Recording::new(Arc::new(adapter)));
    *lock(&rig.recording) = Some(Arc::clone(&recording));
    let key = run_key().ok_or("the OS's randomness failed")?;
    let server = Server::start_real(
        dir,
        RealSetup {
            microsoft: Arc::clone(&recording) as _,
            password,
            key,
        },
    )
    .await?;
    let url = server.url();
    let grpc = server.grpc();
    let mut held = rig.server.lock().await;
    let server = held.insert(server);
    let bundle = server.try_bundle().await?;
    rig.saw(Kind::Bundle, bundle.to_text().expose_secret());
    rig.saw(Kind::EnrollmentToken, bundle.token().expose_secret());
    let plane = enroll_files(
        server,
        dir,
        AGENT,
        &format!("https://127.0.0.1:{}", grpc.port()),
        bundle,
        OsRandom,
        SETUP,
    )
    .await
    // `enroll`'s output names files and the server's address: never shown.
    .map_err(|error| error.why)?;
    drop(held);
    let agent = start_agent(dir, &plane).await?;
    *rig.agent.lock().await = Some(agent);
    Ok(url)
}

/// 24 bytes from the OS, in hex: the Owner's password, only in memory, in
/// buffers wiped when they go; none if the OS's randomness failed.
fn random_password() -> Option<SecretString> {
    let mut bytes = Zeroizing::new([0_u8; 24]);
    OsRandom.fill(bytes.as_mut_slice()).ok()?;
    // Sized for the 48 digits, so writing them never reallocates, and the
    // secret takes the text as it is.
    let mut hex = Zeroizing::new(String::with_capacity(48));
    for byte in bytes.iter() {
        // Writing into a String never fails.
        let _ = write!(hex, "{byte:02x}");
    }
    Some(SecretString::from(core::mem::take(&mut *hex)))
}

/// The vault's key for this run alone, drawn from the OS straight into its
/// box and kept only in memory; none if the OS's randomness failed.
fn run_key() -> Option<SecretBox<[u8; 32]>> {
    let mut drawn = false;
    let key = SecretBox::init_with_mut(|key: &mut [u8; 32]| drawn = OsRandom.fill(key).is_ok());
    drawn.then_some(key)
}

/// The agent as main.rs runs it: `agent.toml` in `dir`, loaded and checked
/// by the agent's own loader, its identity loaded, the azalea connector
/// with the config's target policy.
async fn start_agent(
    dir: &Path,
    plane: &fleet_agent::config::ControlPlaneConfig,
) -> Result<RunningAgent, String> {
    let path = dir.join("agent.toml");
    tokio::fs::write(&path, agent_toml(dir, plane))
        .await
        .map_err(|_| "agent.toml couldn't be written".to_owned())?;
    let config = load_agent_config(&path)?;
    let AgentMode::ControlPlane(plane) = &config.mode else {
        return Err("the agent's config isn't managed".to_owned());
    };
    let identity = ManagedIdentity::load(plane, rustls::pki_types::UnixTime::now())
        .map_err(|_| "the agent's identity doesn't load".to_owned())?;
    let connector = AzaleaConnector::new(&config.mc).with_targets(config.mode.target_policy());
    let (signals, receiver) = mpsc::channel(4);
    let mut seed = [0_u8; 8];
    OsRandom
        .fill(&mut seed)
        .map_err(|_| "the OS's randomness failed".to_owned())?;
    let seed = u64::from_le_bytes(seed);
    let mut task = JoinSet::new();
    task.spawn(run(RunParts {
        config,
        connector: Arc::new(connector.clone()),
        diagnostics: connector,
        signals: Signals(receiver),
        heartbeat: Beats,
        anchor: SystemClock.now(),
        seed,
        managed: Some(Managed {
            identity,
            seed,
            timings: LinkTimings::default(),
        }),
    }));
    Ok(RunningAgent { signals, task })
}

/// `agent.toml` for an agent whose files are in `dir` and `plane`, as an
/// operator writes it.
fn agent_toml(dir: &Path, plane: &ControlPlaneConfig) -> String {
    format!(
        r#"name = "{AGENT}"

[runtime]
heartbeat_file = "{heartbeat}"

[control_plane]
url = "{url}"
ca_cert_file = "{ca}"
cert_file = "{cert}"
key_file = "{key}"
# The check's Minecraft container publishes its game port only on
# 127.0.0.1 (Docker Desktop on Windows forwards it there), and the bot gets
# 127.0.0.1:<port>, never localhost, which may resolve to ::1. Nothing else
# private is listed (ADR-0019, S46, S69, R58).
private_targets = ["127.0.0.1/32"]
"#,
        heartbeat = toml_path(&dir.join("agent.alive")),
        url = plane.url,
        ca = toml_path(&plane.ca_cert_file),
        cert = toml_path(&plane.cert_file),
        key = toml_path(&plane.key_file),
    )
}

/// The config at `path`, through the agent's own loader. A failure is a
/// fixed text, or validation's problems, which name keys and rules but
/// never a value; a parse error's text may hold a path, so it's left out.
fn load_agent_config(path: &Path) -> Result<AgentConfig, String> {
    agent_config::load(path).map_err(|error| match error {
        ConfigError::Invalid(problems) => format!("the agent's config isn't valid: {problems}"),
        ConfigError::Parse(_) => "the agent's config can't be read".to_owned(),
        ConfigError::NotFound { .. } => "agent.toml isn't there".to_owned(),
    })
}

/// `path` as a TOML basic string's content.
fn toml_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
}

/// The Owner's first login: the password, the authenticator's setup, then
/// a recovery code (the next TOTP step is up to 30 s away).
async fn log_in(
    rig: &Rig,
    url: &str,
    password: &SecretString,
) -> Result<(Client<MemoryStore>, MemoryStore), String> {
    let store = MemoryStore::new();
    let client = Client::new(
        ClientConfig {
            server: ServerUrl::parse(url).map_err(|_| "the server's URL".to_owned())?,
            trust: Trust::System,
        },
        store.clone(),
    )
    .map_err(|_| "the client can't be built".to_owned())?;
    let login = || LoginRequest {
        username: SecretText::new(OWNER.to_owned()),
        password: SecretText::new(password.expose_secret().to_owned()),
    };
    match client.login(&login()).await {
        Ok(LoginStep::SetupRequired { .. }) => {}
        Ok(_) => return Err("the Owner already has 2FA".to_owned()),
        Err(error) => return Err(describe(&error)),
    }
    let enrolled = client
        .mfa_enroll()
        .await
        .map_err(|error| describe(&error))?;
    let uri = enrolled.otpauth_uri.expose();
    rig.saw(Kind::TotpSecret, uri);
    let app = Totp::from_url(uri).map_err(|_| "the otpauth URI doesn't read".to_owned())?;
    // The secret alone, as the URI's query holds it.
    if let Some(secret) = uri
        .split(['?', '&'])
        .find_map(|part| part.strip_prefix("secret="))
    {
        rig.saw(Kind::TotpSecret, secret);
    }
    let now = u64::try_from(SystemClock.now().timestamp()).unwrap_or_default();
    let confirmed = client
        .mfa_confirm(SecretText::new(app.generate(now).to_string()))
        .await
        .map_err(|error| describe(&error))?;
    for code in &confirmed.recovery_codes {
        rig.saw(Kind::RecoveryCode, code.expose());
    }
    let recovery = confirmed
        .recovery_codes
        .first()
        .ok_or_else(|| "no recovery code came".to_owned())?
        .expose()
        .to_owned();
    match client.login(&login()).await {
        Ok(LoginStep::MfaRequired { .. }) => {}
        Ok(_) => return Err("the second login didn't ask for a code".to_owned()),
        Err(error) => return Err(describe(&error)),
    }
    client
        .login_mfa(SecretText::new(recovery))
        .await
        .map_err(|error| describe(&error))?;
    Ok((client, store))
}

/// Starts the link, prints its link and code, and waits for its end:
/// the account, linked and active, or none after a FAIL line.
async fn link(
    rig: &Rig,
    client: &Client<MemoryStore>,
    steps: &mut Steps,
) -> Option<fleet_core::id::AccountId> {
    let started = match client.start_link().await {
        Ok(started) => started,
        Err(error) => {
            steps.fail("link started", describe(&error));
            return None;
        }
    };
    rig.saw(Kind::UserCode, started.user_code.expose());
    Steps::pass("link started");
    println!(
        "Open {} and enter the code {}, then sign in with the Microsoft account that owns Minecraft: Java Edition and approve.",
        started.verification_uri,
        started.user_code.expose()
    );
    println!("It waits at most 4 minutes for your approval.");
    let deadline = tokio::time::Instant::now() + LINK_WAIT;
    loop {
        tokio::time::sleep(READ_EVERY).await;
        match client.link_status(started.flow_id).await {
            Ok(status) => match status.status.known() {
                Some(LinkFlowState::Linked) => {
                    let active = status.account_status.as_ref().and_then(Open::known)
                        == Some(AccountStatus::Active);
                    let Some(account) = status.account_id else {
                        steps.fail("linked, active", "the status names no account");
                        return None;
                    };
                    steps.check("linked, active", active, "the account isn't active");
                    return active.then_some(account);
                }
                Some(LinkFlowState::Waiting | LinkFlowState::Finishing) | None => {}
                Some(other) => {
                    steps.fail("linked, active", format!("the link ended {other:?}"));
                    return None;
                }
            },
            Err(error) => {
                steps.fail("linked, active", describe(&error));
                return None;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            steps.fail("linked, active", "no approval within 4 minutes");
            return None;
        }
    }
}

/// The new account's bot, listed with no server yet.
async fn listed(
    client: &Client<MemoryStore>,
    account: fleet_core::id::AccountId,
    steps: &mut Steps,
) -> Option<BotId> {
    let mut before = None;
    loop {
        let page = match client
            .bots(&PageQuery {
                before,
                limit: None,
            })
            .await
        {
            Ok(page) => page,
            Err(error) => {
                steps.fail("the bot listed", describe(&error));
                return None;
            }
        };
        if let Some(bot) = page.items.iter().find(|bot| bot.account.id == account) {
            let unplaced = bot.status.kind.known() == Some(BotStatusKind::Unplaced)
                && bot.status.why.as_ref().and_then(Open::known) == Some(UnplacedReason::NoServer);
            steps.check(
                "the bot listed (unplaced, no_server)",
                unplaced,
                &shown(bot),
            );
            return unplaced.then_some(bot.id);
        }
        let Some(next) = page.next else {
            steps.fail("the bot listed", "the account has no bot");
            return None;
        };
        before = Some(next);
    }
}

/// The container's address, once it's healthy.
async fn minecraft_address(rig: &Rig, steps: &mut Steps) -> Option<String> {
    let by = (*lock(&rig.container_by)).unwrap_or_else(Instant::now);
    let started = tokio::time::timeout_at(by, rig.minecraft.lock().await.join_next()).await;
    match started {
        Ok(Some(Ok(Ok(minecraft)))) => {
            let address = minecraft.address();
            *rig.ready.lock().await = Some(minecraft);
            Steps::pass("Minecraft server started");
            Some(address)
        }
        Ok(Some(Ok(Err(why)))) => {
            steps.fail("Minecraft server started", why);
            None
        }
        _ => {
            steps.fail(
                "Minecraft server started",
                format!("it didn't start within {} s", CONTAINER.as_secs()),
            );
            None
        }
    }
}

/// Steps up, then points the bot at `address`.
async fn configure(
    client: &Client<MemoryStore>,
    password: &SecretString,
    bot: BotId,
    address: &str,
) -> Result<(), String> {
    client
        .step_up(&StepUpRequest {
            password: SecretText::new(password.expose_secret().to_owned()),
        })
        .await
        .map_err(|error| describe(&error))?;
    client
        .patch_bot(
            bot,
            &BotPatchRequest {
                server_address: address.to_owned(),
            },
        )
        .await
        .map_err(|error| describe(&error))?;
    Ok(())
}

/// Waits for the bot to be reported Online; prints the states it saw.
async fn online(client: &Client<MemoryStore>, bot: BotId, steps: &mut Steps) -> bool {
    let deadline = tokio::time::Instant::now() + ONLINE_WAIT;
    let mut seen: Vec<String> = Vec::new();
    loop {
        tokio::time::sleep(READ_EVERY).await;
        let read = match client.bot(bot).await {
            Ok(read) => read,
            Err(error) => {
                steps.fail("Online", describe(&error));
                return false;
            }
        };
        let now = shown(&read);
        if seen.last() != Some(&now) {
            seen.push(now.clone());
        }
        let state = read.status.state.as_ref().and_then(Open::known);
        if read.status.kind.known() == Some(BotStatusKind::Reported)
            && state == Some(BotStateKind::Online)
        {
            Steps::pass("Online");
            println!("  states seen, for information: {}", seen.join(" → "));
            return true;
        }
        if state == Some(BotStateKind::Failed) {
            steps.fail("Online", format!("the bot failed: {now}"));
            return false;
        }
        if tokio::time::Instant::now() >= deadline {
            steps.fail("Online", format!("not within 90 s; last {now}"));
            println!("  states seen: {}", seen.join(" → "));
            return false;
        }
    }
}

/// Waits for the stopped bot to be off its agent.
async fn offline(client: &Client<MemoryStore>, bot: BotId, steps: &mut Steps) {
    let deadline = tokio::time::Instant::now() + OFFLINE_WAIT;
    loop {
        tokio::time::sleep(READ_EVERY).await;
        let read = match client.bot(bot).await {
            Ok(read) => read,
            Err(error) => {
                steps.fail("offline", describe(&error));
                return;
            }
        };
        if read.status.kind.known() == Some(BotStatusKind::Unplaced)
            && read.status.why.as_ref().and_then(Open::known) == Some(UnplacedReason::Stopped)
        {
            Steps::pass("offline (unplaced, stopped)");
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            steps.fail("offline", format!("not within 60 s; last {}", shown(&read)));
            return;
        }
    }
}

/// A bot's status in enumerated words only: its kind, state, reason, why,
/// and the kind of its last disconnect; never its server or its name.
fn shown(bot: &BotResponse) -> String {
    let status = &bot.status;
    let mut text = status.kind.as_str().to_owned();
    if let Some(state) = &status.state {
        text.push('/');
        text.push_str(state.as_str());
    }
    if let Some(reason) = &status.reason {
        text.push_str(" reason=");
        text.push_str(reason.as_str());
    }
    if let Some(why) = &status.why {
        text.push_str(" why=");
        text.push_str(why.as_str());
    }
    if let Some(last) = &status.last_disconnect {
        // Its kind and its enumerated causes; a kick's key and message are
        // the Minecraft server's text, so they're left out.
        text.push_str(" last_disconnect=");
        text.push_str(last.kind.as_str());
        if let Some(failure) = &last.connect_failure {
            text.push_str(" connect_failure=");
            text.push_str(failure.as_str());
            if failure.as_str() == "blocked" {
                text.push_str(" (the agent's private_targets refused the address)");
            }
        }
        if let Some(restriction) = &last.restriction {
            text.push_str(" restriction=");
            text.push_str(restriction.as_str());
        }
        if let Some(failure) = &last.session_server_failure {
            text.push_str(" session_server_failure=");
            text.push_str(failure.as_str());
        }
    }
    text
}

/// A client error as its status and code, never its text.
fn describe(error: &ClientError) -> String {
    error.api().map_or_else(
        || "the request didn't get an answer".to_owned(),
        |failure| format!("{} {}", failure.status, failure.code.as_str()),
    )
}

/// Every end: the agent, the server, the container, then the keys'
/// bodies are read for the log check before the folder goes.
async fn tear_down(rig: &Rig, dir: &Path, steps: &mut Steps) {
    if let Some(mut agent) = rig.agent.lock().await.take() {
        // Nothing else sends, so there's room.
        let _ = agent.signals.try_send(Signal::Terminate);
        let stopped = tokio::time::timeout(AGENT_STOP, agent.task.join_next()).await;
        let clean = matches!(&stopped, Ok(Some(Ok(outcome))) if outcome.exit == Exit::Stopped);
        steps.check(
            "agent stopped",
            clean,
            &format!("it didn't stop cleanly within {} s", AGENT_STOP.as_secs()),
        );
    }
    if let Some(mut server) = rig.server.lock().await.take() {
        let clean = server.try_stop(SERVER_STOP).await;
        steps.check(
            "server stopped",
            clean,
            &format!("it didn't stop cleanly within {} s", SERVER_STOP.as_secs()),
        );
    }
    // A start still under way ends first, by its spawn's bound, so its
    // container is removed too.
    let by = *lock(&rig.container_by);
    let mut starting = rig.minecraft.lock().await;
    if let Some(by) = by {
        while let Ok(Some(started)) = tokio::time::timeout_at(by, starting.join_next()).await {
            if let Ok(Ok(minecraft)) = started {
                *rig.ready.lock().await = Some(minecraft);
            }
        }
    }
    if !starting.is_empty() {
        starting.abort_all();
        steps.fail(
            "container removed",
            "its start didn't end: find it with `docker ps -a`, then `docker rm -fv` it",
        );
    }
    drop(starting);
    if let Some(minecraft) = rig.ready.lock().await.take() {
        match minecraft.remove(REMOVE).await {
            Ok(()) => Steps::pass("container removed"),
            Err(why) => steps.fail(
                "container removed",
                format!(
                    "{why} (within {} s): remove it by hand: find it with `docker ps -a`, then `docker rm -fv` it",
                    REMOVE.as_secs()
                ),
            ),
        }
    }
    let mut secrets = Secrets::default();
    if let Some(recording) = lock(&rig.recording).take() {
        secrets.extend(recording.take());
    }
    for key in [
        "pki/ca.key",
        "pki/server.key",
        &format!("{AGENT}/agent.key"),
    ] {
        // Wiped when it goes, its whole capacity included.
        let Ok(bytes) = tokio::fs::read(dir.join(key)).await.map(Zeroizing::new) else {
            continue;
        };
        let Ok(text) = core::str::from_utf8(&bytes) else {
            continue;
        };
        for line in text
            .lines()
            .filter(|line| line.len() >= 40 && !line.starts_with("-----"))
        {
            secrets.saw(Kind::Key, line);
        }
    }
    lock(&rig.secrets).extend(secrets);
}

/// A heartbeat nobody reads.
#[derive(Debug, Clone, Copy)]
struct Beats;

impl Heartbeat for Beats {
    fn touch(&self) -> impl Future<Output = io::Result<()>> + Send {
        core::future::ready(Ok(()))
    }
}

/// The check's signals.
#[derive(Debug)]
struct Signals(mpsc::Receiver<Signal>);

impl ShutdownSignals for Signals {
    async fn recv(&mut self) -> Signal {
        match self.0.recv().await {
            Some(signal) => signal,
            None => core::future::pending().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use fleet_core::net::PrivateTargets;
    use fleet_testkit::jail::in_jail;

    use fleet_core::audit::AuditAction;

    use super::*;
    use crate::harness::logs::{Logs, PATIENCE};

    /// The check's own setup, login and teardown, on the real path but for
    /// Microsoft and Docker: the server on the real clock and the OS's
    /// randomness, the agent enrolled and run from its `agent.toml` with the
    /// azalea connector, the Owner through 2FA. No account is linked, so no
    /// bot runs and Microsoft is never asked.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_checks_setup_login_and_teardown_pass_without_an_account() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        log_capture::install().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let rig = Rig::default();
        let password = random_password().unwrap();
        rig.saw(Kind::Password, password.expose_secret());
        // Only for its bounded wait: every line goes to the capture.
        let logs = Logs::default();

        let ran = tokio::time::timeout(PATIENCE, async {
            let url = set_up(&rig, dir.path(), password.clone()).await?;
            log_in(&rig, &url, &password).await.map(drop)
        })
        .await;
        if let (Ok(Ok(())), Some(server)) = (&ran, rig.server.lock().await.as_ref()) {
            // The agent's identity holds over mTLS: the server welcomed it.
            logs.until("the agent connected", async || {
                !server.entries(&AuditAction::AGENT_CONNECT).await.is_empty()
            })
            .await;
        }
        let mut teardown = Steps::default();
        tear_down(&rig, dir.path(), &mut teardown).await;
        let findings = lock(&rig.secrets).findings();

        assert_eq!(ran, Ok(Ok(())), "set up and logged in");
        assert_eq!(teardown.failures, Vec::<String>::new());
        // The password, the TOTP secret, the recovery codes, the bundle, its
        // token, the refresh token and the three keys, and every token by
        // its shape.
        assert_eq!(findings, Vec::<String>::new(), "no secret in any log");
    }

    /// A fake secret, as a test plants it: never a real one.
    const PLANTED: &str = "planted-0c4e8a1f6b2d93";

    #[tokio::test]
    async fn a_panic_in_the_steps_still_runs_the_log_check_and_then_goes_on() {
        log_capture::install().unwrap();
        tracing::warn!(target: "fleet_planted", "a line that leaks {PLANTED}");
        let mut secrets = Secrets::default();
        secrets.saw(Kind::MinecraftToken, PLANTED);
        let panicked = tokio::spawn(async { panic!("a step panicked") }).await;

        let (steps, panic) = conclude(
            Ok(panicked.map(|()| Steps::default())),
            Steps::default(),
            true,
            &secrets,
        );

        assert!(panic.is_some(), "the panic goes on");
        assert_eq!(
            steps.failures,
            ["no secret in any log: a Minecraft token: WARN fleet_planted (1 lines)"]
        );
    }

    #[tokio::test]
    async fn teardown_goes_on_though_a_panicking_step_poisoned_its_locks() {
        let dir = tempfile::tempdir().unwrap();
        let rig = Arc::new(Rig::default());
        let step = Arc::clone(&rig);
        let panicked = std::thread::spawn(move || {
            let _held = (
                step.secrets.lock().unwrap(),
                step.recording.lock().unwrap(),
                step.container_by.lock().unwrap(),
            );
            panic!("a step panicked holding the locks");
        })
        .join();
        assert!(panicked.is_err() && rig.secrets.is_poisoned());
        let mut teardown = Steps::default();

        tear_down(&rig, dir.path(), &mut teardown).await;

        assert_eq!(teardown.failures, Vec::<String>::new());
    }

    #[test]
    fn the_agents_config_loads_as_the_check_writes_it() {
        in_jail(|jail| {
            let dir = jail.directory().to_owned();
            let plane = ControlPlaneConfig {
                url: "https://127.0.0.1:7443".to_owned(),
                ca_cert_file: dir.join(AGENT).join("ca.crt"),
                cert_file: dir.join(AGENT).join("agent.crt"),
                key_file: dir.join(AGENT).join("agent.key"),
                private_targets: PrivateTargets::default(),
            };
            let path = dir.join("agent.toml");
            std::fs::write(&path, agent_toml(&dir, &plane)).unwrap();

            let loaded = load_agent_config(&path);

            let config = loaded.unwrap();
            let AgentMode::ControlPlane(loaded) = &config.mode else {
                panic!("a managed agent");
            };
            assert_eq!(loaded.url, plane.url);
            assert_eq!(loaded.key_file, plane.key_file);
            assert_eq!(config.heartbeat_file, dir.join("agent.alive"));
            assert_eq!(loaded.private_targets.networks().len(), 1);
            assert!(
                loaded
                    .private_targets
                    .contains(core::net::IpAddr::from([127, 0, 0, 1]))
            );
        });
    }
}
