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
//! link's wait; always inside the `manual` profile's 10 minutes.

mod minecraft;
mod pins;
mod recording;
mod secrets;

use core::fmt::{Display, Write as _};
use core::time::Duration;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fleet_agent::config::{self as agent_config, AgentMode};
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
use secrecy::{ExposeSecret as _, SecretBox, SecretString};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinSet;
use totp_rs::Totp;

use self::minecraft::{Minecraft, STARTUP};
use self::recording::Recording;
use self::secrets::{Kind, Secrets};
use crate::harness::agent::enroll_files;
use crate::harness::server::{OWNER, RealSetup, Server};

/// The longest wait for the user to approve the sign-in.
const LINK_WAIT: Duration = Duration::from_mins(4);
/// The longest wait for the bot to go Online once started.
const ONLINE_WAIT: Duration = Duration::from_secs(90);
/// The longest wait for the bot to be off its agent once stopped.
const OFFLINE_WAIT: Duration = Duration::from_secs(60);
/// How often the link and the bot are read.
const READ_EVERY: Duration = Duration::from_secs(2);
/// The longest wait for the agent or the server to stop.
const STOP_WAIT: Duration = Duration::from_secs(45);

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

    /// Checks every log line for every secret the run saw, then fails the
    /// test if any step failed.
    fn finish(mut self, secrets: &Secrets) {
        let findings = secrets.findings();
        if findings.is_empty() {
            Self::pass("no secret in any log");
        } else {
            for finding in findings {
                self.fail("no secret in any log", finding);
            }
        }
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
    ready: Mutex<Option<Minecraft>>,
    recording: std::sync::Mutex<Option<Arc<Recording>>>,
    secrets: std::sync::Mutex<Secrets>,
}

impl Rig {
    fn saw(&self, kind: Kind, value: &str) {
        self.secrets.lock().unwrap().saw(kind, value);
    }
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
    println!("The check takes at most about 9 minutes.");
    let dir = tempfile::Builder::new()
        .prefix("afkfleet-managed-check-")
        .tempdir()
        .unwrap();
    let rig = Arc::new(Rig::default());

    // The steps run in a task of their own, so a panic comes back here as an
    // error: everything stops and the folder goes on every end.
    let ended = tokio::spawn(check(Arc::clone(&rig), dir.path().to_owned())).await;
    let mut teardown = Steps::default();
    tear_down(&rig, dir.path(), &mut teardown).await;
    let secrets = core::mem::take(&mut *rig.secrets.lock().unwrap());
    drop(rig);
    let deleted = dir.close().is_ok();
    match ended {
        Ok(mut steps) => {
            steps.failures.append(&mut teardown.failures);
            steps.check(
                "temporary folder deleted",
                deleted,
                "it's still there: delete the afkfleet-managed-check folder in the temp directory",
            );
            steps.finish(&secrets);
        }
        Err(failed) => {
            if !deleted {
                println!(
                    "temporary folder deleted: FAIL (delete the afkfleet-managed-check folder in the temp directory)"
                );
            }
            std::panic::resume_unwind(failed.into_panic());
        }
    }
}

/// The check's steps; the first that fails ends them.
async fn check(rig: Arc<Rig>, dir: PathBuf) -> Steps {
    let mut steps = Steps::default();
    let password = random_password();
    rig.saw(Kind::Password, password.expose_secret());

    // The server, its Microsoft adapter recorded, and the agent.
    let Ok(adapter) = ReqwestMicrosoftAuth::new(Arc::new(SystemClock)) else {
        steps.fail(
            "server and agent started",
            "Microsoft's HTTP client can't be built",
        );
        return steps;
    };
    let recording = Arc::new(Recording::new(Arc::new(adapter)));
    *rig.recording.lock().unwrap() = Some(Arc::clone(&recording));
    let server = Server::start_real(
        &dir,
        RealSetup {
            microsoft: Arc::clone(&recording) as _,
            password: password.clone(),
            key: run_key(),
        },
    )
    .await;
    let url = server.url();
    let grpc = server.grpc();
    let bundle = server.bundle().await;
    rig.saw(Kind::Bundle, bundle.to_text().expose_secret());
    let plane = enroll_files(
        &server,
        &dir,
        AGENT,
        &format!("https://127.0.0.1:{}", grpc.port()),
        bundle,
        OsRandom,
    )
    .await;
    *rig.server.lock().await = Some(server);
    match start_agent(&dir, &plane).await {
        Ok(agent) => *rig.agent.lock().await = Some(agent),
        Err(why) => {
            steps.fail("server and agent started", why);
            return steps;
        }
    }
    Steps::pass("server and agent started (enrolled)");

    // The container starts while the Owner logs in and the link waits.
    rig.minecraft.lock().await.spawn(Minecraft::start());

    let (client, store) = match log_in(&rig, &url, &password).await {
        Ok(logged_in) => logged_in,
        Err(why) => {
            steps.fail("Owner logged in with 2FA", why);
            return steps;
        }
    };
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
    // The client's refresh token, which no log may hold either.
    if let Ok(Some(token)) = fleet_client::SessionStore::load(&store).await {
        rig.saw(Kind::RefreshToken, token.expose_secret());
    }
    steps
}

/// 24 bytes from the OS, in hex: the Owner's password, only in memory.
fn random_password() -> SecretString {
    let mut bytes = [0_u8; 24];
    OsRandom.fill(&mut bytes).unwrap();
    let hex = bytes.iter().fold(String::new(), |mut hex, byte| {
        // Writing into a String never fails.
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    bytes.fill(0);
    SecretString::from(hex)
}

/// The vault's key for this run alone, drawn from the OS straight into its
/// box and kept only in memory.
fn run_key() -> SecretBox<[u8; 32]> {
    SecretBox::init_with_mut(|key: &mut [u8; 32]| OsRandom.fill(key).unwrap())
}

/// The agent as main.rs runs it: `agent.toml` in `dir`, loaded and checked
/// by the agent's own loader, its identity loaded, the azalea connector
/// with the config's target policy.
async fn start_agent(
    dir: &Path,
    plane: &fleet_agent::config::ControlPlaneConfig,
) -> Result<RunningAgent, String> {
    let path = dir.join("agent.toml");
    let toml = format!(
        r#"name = "{AGENT}"
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
    );
    tokio::fs::write(&path, toml)
        .await
        .map_err(|_| "agent.toml couldn't be written".to_owned())?;
    let config =
        agent_config::load(&path).map_err(|_| "the agent's config isn't valid".to_owned())?;
    let AgentMode::ControlPlane(plane) = &config.mode else {
        return Err("the agent's config isn't managed".to_owned());
    };
    let identity = ManagedIdentity::load(plane, rustls::pki_types::UnixTime::now())
        .map_err(|_| "the agent's identity doesn't load".to_owned())?;
    let connector = AzaleaConnector::new(&config.mc).with_targets(config.mode.target_policy());
    let (signals, receiver) = mpsc::channel(4);
    let mut seed = [0_u8; 8];
    OsRandom.fill(&mut seed).unwrap();
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
    let started = tokio::time::timeout(STARTUP, rig.minecraft.lock().await.join_next()).await;
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
                "it didn't start within 5 minutes",
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
        let _ = agent.signals.send(Signal::Terminate).await;
        let stopped = tokio::time::timeout(STOP_WAIT, agent.task.join_next()).await;
        let clean = matches!(&stopped, Ok(Some(Ok(outcome))) if outcome.exit == Exit::Stopped);
        steps.check("agent stopped", clean, "it didn't stop cleanly");
    }
    if let Some(mut server) = rig.server.lock().await.take() {
        tokio::time::timeout(STOP_WAIT, server.stop())
            .await
            .map_or_else(
                |_| steps.fail("server stopped", "not within 45 s"),
                |()| Steps::pass("server stopped"),
            );
    }
    // A start still under way ends first, so its container is removed too.
    let mut starting = rig.minecraft.lock().await;
    while let Ok(Some(started)) = tokio::time::timeout(STARTUP, starting.join_next()).await {
        if let Ok(Ok(minecraft)) = started {
            *rig.ready.lock().await = Some(minecraft);
        }
    }
    drop(starting);
    if let Some(minecraft) = rig.ready.lock().await.take() {
        let removed = minecraft.remove().await;
        steps.check(
            "container removed",
            removed,
            "find it with `docker ps -a`, then `docker rm -fv` it",
        );
    }
    let mut secrets = Secrets::default();
    if let Some(recording) = rig.recording.lock().unwrap().take() {
        secrets.extend(recording.take());
    }
    for key in [
        "pki/ca.key",
        "pki/server.key",
        &format!("{AGENT}/agent.key"),
    ] {
        if let Ok(text) = tokio::fs::read_to_string(dir.join(key)).await {
            for line in text
                .lines()
                .filter(|line| line.len() >= 40 && !line.starts_with("-----"))
            {
                secrets.saw(Kind::Key, line);
            }
        }
    }
    rig.secrets.lock().unwrap().extend(secrets);
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
