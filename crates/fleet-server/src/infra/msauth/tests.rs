//! The adapter against local fakes (ADR-0018). CI never calls Microsoft.
//! - An axum fake on `127.0.0.1:0` scripts each endpoint's status and body
//!   and records every request.
//! - Raw TCP servers on threads send what axum can't (a false
//!   `Content-Length`, an endless body, garbage instead of TLS).
//! - A rustls server with an rcgen CA, made at run time since no key is
//!   ever committed, presents a certificate the platform doesn't trust.
//!
//! Real time with short bounds, never a sleep: sockets and paused time
//! don't mix.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener};
use std::sync::Mutex;
use std::thread;

use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use chrono::{DateTime, Utc};
use fleet_core::time::add;
use fleet_testkit::log_capture;
use fleet_testkit::system::ManualClock;
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use secrecy::SecretString;

use super::*;
use crate::ports::msauth::{MAX_ANSWER_BYTES, XboxRefusal};

/// How long a raw server waits for the client before giving up.
const SOCKET_WAIT: Duration = Duration::from_secs(5);

const DEVICE_PATH: &str = "/oauth20_connect.srf";
const TOKEN_PATH: &str = "/oauth20_token.srf";
const XBOX_PATH: &str = "/user/authenticate";
const XSTS_PATH: &str = "/xsts/authorize";
const LOGIN_PATH: &str = "/authentication/login_with_xbox";
const ENTITLEMENTS_PATH: &str = "/entitlements/mcstore";
const PROFILE_PATH: &str = "/minecraft/profile";

/// Bounds for calls that should succeed or fail on their own. The connect
/// bound is the production one: Windows takes about 2 s to report a refused
/// connection.
fn quick() -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(5),
        request: Duration::from_secs(8),
        call: Duration::from_secs(10),
        session: Duration::from_secs(10),
    }
}

fn start() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).unwrap()
}

fn after(secs: u64) -> DateTime<Utc> {
    add(start(), Duration::from_secs(secs))
}

// --- Answers ---------------------------------------------------------------

fn device_answer(device_code: &str, user_code: &str) -> String {
    format!(
        r#"{{"user_code":"{user_code}","device_code":"{device_code}","verification_uri":"https://www.microsoft.com/link","expires_in":900,"interval":5,"message":"text"}}"#
    )
}

fn token_answer(access: &str, refresh: &str) -> String {
    format!(
        r#"{{"token_type":"bearer","expires_in":3600,"scope":"service::user.auth.xboxlive.com::MBI_SSL","access_token":"{access}","refresh_token":"{refresh}","user_id":"x"}}"#
    )
}

fn oauth_error(code: &str) -> String {
    format!(r#"{{"error":"{code}","error_description":"text"}}"#)
}

fn xbox_answer(token: &str, user_hash: &str) -> String {
    format!(
        r#"{{"IssueInstant":"2026-10-14T00:00:00Z","NotAfter":"2026-10-28T00:00:00Z","Token":"{token}","DisplayClaims":{{"xui":[{{"uhs":"{user_hash}"}}]}}}}"#
    )
}

fn login_answer(token: &str) -> String {
    format!(
        r#"{{"username":"x","roles":[],"access_token":"{token}","token_type":"Bearer","expires_in":86400}}"#
    )
}

const OWNED: &str = r#"{"items":[{"name":"product_minecraft","signature":"x"},{"name":"game_minecraft","signature":"x"}],"signature":"x","keyId":"1"}"#;

fn profile_answer(id: &str, name: &str) -> String {
    format!(r#"{{"id":"{id}","name":"{name}","skins":[],"capes":[]}}"#)
}

fn secret(text: &str) -> SecretString {
    SecretString::from(text.to_owned())
}

// --- The axum fake ---------------------------------------------------------

/// What the fake answers a request with.
#[derive(Debug, Clone)]
enum Reply {
    /// This status and body.
    Answer(u16, String),
    /// Nothing, ever.
    Hang,
    /// A 302 to this path.
    RedirectTo(&'static str),
}

/// A request the fake received.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    query: Option<String>,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Seen {
    /// The body as a form, decoded.
    fn form(&self) -> HashMap<String, String> {
        let text = String::from_utf8(self.body.clone()).unwrap();
        reqwest::Url::parse(&format!("http://fake.example.com/?{text}"))
            .unwrap()
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    }

    /// The body as JSON.
    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap()
    }

    fn header(&self, name: &str) -> &str {
        self.headers[name].to_str().unwrap()
    }
}

#[derive(Debug, Default)]
struct Script {
    replies: HashMap<String, VecDeque<Reply>>,
    seen: Vec<Seen>,
    /// A clock the fake moves forward on every request, to show when the
    /// adapter read it.
    advance: Option<(ManualClock, Duration)>,
}

/// The fake on `127.0.0.1:0`; dropping it stops the server.
struct Fake {
    address: SocketAddr,
    script: Arc<Mutex<Script>>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fake {
    async fn start() -> Self {
        let script = Arc::new(Mutex::new(Script::default()));
        let router = Router::new()
            .fallback(handle)
            .with_state(Arc::clone(&script));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Self {
            address,
            script,
            server,
        }
    }

    fn reply(&self, path: &str, reply: Reply) {
        self.script
            .lock()
            .unwrap()
            .replies
            .entry(path.to_owned())
            .or_default()
            .push_back(reply);
    }

    fn answer(&self, path: &str, status: u16, body: &str) {
        self.reply(path, Reply::Answer(status, body.to_owned()));
    }

    fn advance_on_each_request(&self, clock: &ManualClock, by: Duration) {
        self.script.lock().unwrap().advance = Some((clock.clone(), by));
    }

    fn seen(&self) -> Vec<Seen> {
        self.script.lock().unwrap().seen.clone()
    }

    fn paths(&self) -> Vec<String> {
        self.seen().into_iter().map(|seen| seen.path).collect()
    }

    fn base(&self) -> String {
        format!("http://{}", self.address)
    }

    fn adapter(&self, timeouts: Timeouts, clock: &ManualClock) -> ReqwestMicrosoftAuth {
        ReqwestMicrosoftAuth::with_endpoints(
            Endpoints::at(&self.base()),
            timeouts,
            Arc::new(clock.clone()),
        )
        .unwrap()
    }

    /// Scripts a whole successful `minecraft_session`.
    fn script_session(
        &self,
        xbox: &str,
        uhs: &str,
        xsts: &str,
        minecraft: &str,
        id: &str,
        name: &str,
    ) {
        self.answer(XBOX_PATH, 200, &xbox_answer(xbox, uhs));
        self.answer(XSTS_PATH, 200, &xbox_answer(xsts, uhs));
        self.answer(LOGIN_PATH, 200, &login_answer(minecraft));
        self.answer(ENTITLEMENTS_PATH, 200, OWNED);
        self.answer(PROFILE_PATH, 200, &profile_answer(id, name));
    }
}

async fn handle(State(script): State<Arc<Mutex<Script>>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, usize::MAX)
        .await
        .unwrap_or_default()
        .to_vec();
    let reply = {
        let mut script = script.lock().unwrap();
        script.seen.push(Seen {
            method: parts.method.to_string(),
            path: parts.uri.path().to_owned(),
            query: parts.uri.query().map(str::to_owned),
            headers: parts.headers.clone(),
            body,
        });
        if let Some((clock, by)) = &script.advance {
            clock.advance(*by);
        }
        script
            .replies
            .get_mut(parts.uri.path())
            .and_then(VecDeque::pop_front)
    };
    match reply {
        Some(Reply::Answer(status, body)) => {
            (StatusCode::from_u16(status).unwrap(), body).into_response()
        }
        Some(Reply::Hang) => std::future::pending().await,
        Some(Reply::RedirectTo(path)) => {
            (StatusCode::FOUND, [(header::LOCATION, path)], "").into_response()
        }
        None => (StatusCode::IM_A_TEAPOT, "unscripted").into_response(),
    }
}

// --- Raw servers -----------------------------------------------------------

/// Reads one request (its head, then as many body bytes as its
/// `Content-Length` says) from `stream`.
fn read_request(stream: &std::net::TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut length = 0;
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
        line.clear();
    }
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
}

/// A server on a thread that answers one request with `answer`, the raw
/// bytes of a whole HTTP response.
fn answering(answer: Vec<u8>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        stream.set_read_timeout(Some(SOCKET_WAIT)).unwrap();
        stream.set_write_timeout(Some(SOCKET_WAIT)).unwrap();
        read_request(&stream);
        let _ = stream.write_all(&answer);
        let _ = stream.flush();
    });
    address
}

/// A server on a thread that accepts one connection and never answers.
fn silent() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        stream.set_read_timeout(Some(SOCKET_WAIT)).unwrap();
        let mut buffer = [0; 1024];
        while stream.read(&mut buffer).is_ok_and(|read| read > 0) {}
    });
    address
}

/// A server on a thread that answers a TLS client hello with plain text.
fn garbage() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        stream.set_read_timeout(Some(SOCKET_WAIT)).unwrap();
        stream.set_write_timeout(Some(SOCKET_WAIT)).unwrap();
        let mut buffer = [0; 1024];
        let _ = stream.read(&mut buffer);
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let _ = stream.flush();
    });
    address
}

/// A CA named `name`.
fn ca(name: &str) -> CertifiedIssuer<'static, KeyPair> {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name.push(DnType::CommonName, name);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap()
}

/// A TLS server on a thread that presents a certificate for `127.0.0.1`
/// from a CA nobody trusts. The refused handshake ends the thread.
fn untrusted_tls() -> SocketAddr {
    let issuer = ca("afkfleet test CA");
    let mut params = CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
    params
        .distinguished_name
        .push(DnType::CommonName, "afkfleet test server");
    let key = KeyPair::generate().unwrap();
    let certificate: CertificateDer<'static> =
        params.signed_by(&key, &*issuer).unwrap().der().clone();
    let key: PrivateKeyDer<'static> = PrivatePkcs8KeyDer::from(key.serialize_der()).into();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![certificate], key)
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        stream.set_read_timeout(Some(SOCKET_WAIT)).unwrap();
        stream.set_write_timeout(Some(SOCKET_WAIT)).unwrap();
        let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut tls = rustls::StreamOwned::new(connection, stream);
        let mut buffer = [0; 1024];
        let _ = tls.read(&mut buffer);
    });
    address
}

/// `call`, which must end on its own within 5 s: a test of a bound fails
/// instead of hanging when the bound is broken.
async fn within<T>(call: impl core::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .expect("the call ended within 5 s")
}

fn raw_adapter(address: SocketAddr, scheme: &str, timeouts: Timeouts) -> ReqwestMicrosoftAuth {
    ReqwestMicrosoftAuth::with_endpoints(
        Endpoints::at(&format!("{scheme}://{address}")),
        timeouts,
        Arc::new(ManualClock::new(start())),
    )
    .unwrap()
}

// --- The four calls --------------------------------------------------------

#[tokio::test]
async fn start_device_flow_sends_azaleas_form_and_returns_the_flow() {
    let fake = Fake::start().await;
    fake.answer(
        DEVICE_PATH,
        200,
        &device_answer("invented-device-code", "B7X4KDLQ"),
    );
    let clock = ManualClock::new(start());

    let flow = fake
        .adapter(quick(), &clock)
        .start_device_flow()
        .await
        .unwrap();

    assert_eq!(flow.code.expose_secret(), "invented-device-code");
    assert_eq!(flow.user_code.as_str(), "B7X4KDLQ");
    assert_eq!(
        flow.verification_uri.as_str(),
        "https://www.microsoft.com/link"
    );
    assert_eq!(flow.expires_at, after(900));
    assert_eq!(flow.interval.get(), Duration::from_secs(5));
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].path, DEVICE_PATH);
    assert_eq!(seen[0].query, None);
    assert_eq!(
        seen[0].header("content-type"),
        "application/x-www-form-urlencoded"
    );
    assert_eq!(
        seen[0].form(),
        HashMap::from([
            ("scope".to_owned(), SCOPE.to_owned()),
            ("client_id".to_owned(), CLIENT_ID.to_owned()),
            ("response_type".to_owned(), "device_code".to_owned()),
        ])
    );
    assert!(!seen[0].headers.contains_key("user-agent"));
}

#[tokio::test]
async fn poll_sends_the_device_code_with_the_client_id_in_the_query() {
    let fake = Fake::start().await;
    fake.answer(TOKEN_PATH, 400, &oauth_error("authorization_pending"));
    let clock = ManualClock::new(start());
    let code = DeviceCode::try_from(secret("invented-device-code")).unwrap();

    let outcome = fake.adapter(quick(), &clock).poll(&code).await.unwrap();

    assert!(matches!(outcome, PollOutcome::Pending), "{outcome:?}");
    let seen = fake.seen();
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].path, TOKEN_PATH);
    assert_eq!(seen[0].query.as_deref(), Some("client_id=00000000441cc96b"));
    assert_eq!(
        seen[0].form(),
        HashMap::from([
            ("client_id".to_owned(), CLIENT_ID.to_owned()),
            ("device_code".to_owned(), "invented-device-code".to_owned()),
            (
                "grant_type".to_owned(),
                "urn:ietf:params:oauth:grant-type:device_code".to_owned()
            ),
        ])
    );
}

#[tokio::test]
async fn an_approved_poll_returns_the_tokens() {
    let fake = Fake::start().await;
    fake.answer(
        TOKEN_PATH,
        200,
        &token_answer("invented-access", "invented-refresh"),
    );
    let clock = ManualClock::new(start());
    let code = DeviceCode::try_from(secret("invented-device-code")).unwrap();

    let outcome = fake.adapter(quick(), &clock).poll(&code).await.unwrap();

    let PollOutcome::Authorized(tokens) = outcome else {
        panic!("expected the tokens, got {outcome:?}");
    };
    assert_eq!(tokens.access.expose_secret(), "invented-access");
    assert_eq!(tokens.refresh.expose_secret(), "invented-refresh");
    assert_eq!(tokens.access_expires_at, after(3_600));
}

#[tokio::test]
async fn refresh_sends_the_refresh_token_with_azaleas_scope() {
    let fake = Fake::start().await;
    fake.answer(
        TOKEN_PATH,
        200,
        &token_answer("invented-access-2", "invented-refresh-2"),
    );
    let clock = ManualClock::new(start());
    let token = RefreshToken::try_from(secret("invented-refresh-1")).unwrap();

    let tokens = fake.adapter(quick(), &clock).refresh(&token).await.unwrap();

    assert_eq!(tokens.access.expose_secret(), "invented-access-2");
    assert_eq!(tokens.refresh.expose_secret(), "invented-refresh-2");
    let seen = fake.seen();
    assert_eq!(seen[0].path, TOKEN_PATH);
    assert_eq!(seen[0].query, None);
    assert_eq!(
        seen[0].form(),
        HashMap::from([
            ("scope".to_owned(), SCOPE.to_owned()),
            ("client_id".to_owned(), CLIENT_ID.to_owned()),
            ("grant_type".to_owned(), "refresh_token".to_owned()),
            ("refresh_token".to_owned(), "invented-refresh-1".to_owned()),
        ])
    );
}

#[tokio::test]
async fn invalid_grant_on_refresh_is_its_own_error() {
    let fake = Fake::start().await;
    fake.answer(TOKEN_PATH, 400, &oauth_error("invalid_grant"));
    let clock = ManualClock::new(start());
    let token = RefreshToken::try_from(secret("invented-refresh-1")).unwrap();

    let error = fake
        .adapter(quick(), &clock)
        .refresh(&token)
        .await
        .unwrap_err();

    assert_eq!(error, RefreshError::InvalidGrant);
}

#[tokio::test]
async fn minecraft_session_runs_its_five_requests_in_order() {
    let fake = Fake::start().await;
    fake.script_session(
        "invented-xbox",
        "1234567890",
        "invented-xsts",
        "invented-minecraft",
        "4f1c9a2e7b3d4e5f8a6b0c1d2e3f4a5b",
        "AfkBot1",
    );
    let clock = ManualClock::new(start());
    let access = MsAccessToken::try_from(secret("invented-access")).unwrap();

    let session = fake
        .adapter(quick(), &clock)
        .minecraft_session(&access)
        .await
        .unwrap();

    assert_eq!(session.token.expose_secret(), "invented-minecraft");
    assert_eq!(session.expires_at, after(86_400));
    assert_eq!(
        session.uuid.to_string(),
        "4f1c9a2e-7b3d-4e5f-8a6b-0c1d2e3f4a5b"
    );
    assert_eq!(session.name.as_str(), "AfkBot1");
    let seen = fake.seen();
    assert_eq!(
        fake.paths(),
        [
            XBOX_PATH,
            XSTS_PATH,
            LOGIN_PATH,
            ENTITLEMENTS_PATH,
            PROFILE_PATH
        ]
    );

    let xbox = &seen[0];
    assert_eq!(xbox.method, "POST");
    assert_eq!(xbox.header("content-type"), "application/json");
    assert_eq!(xbox.header("accept"), "application/json");
    assert_eq!(xbox.header("x-xbl-contract-version"), "1");
    assert_eq!(
        xbox.json(),
        serde_json::json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                "RpsTicket": "invented-access",
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT",
        })
    );

    let xsts = &seen[1];
    assert_eq!(xsts.method, "POST");
    assert_eq!(xsts.header("content-type"), "application/json");
    assert_eq!(xsts.header("accept"), "application/json");
    assert_eq!(
        xsts.json(),
        serde_json::json!({
            "Properties": {
                "SandboxId": "RETAIL",
                "UserTokens": ["invented-xbox"],
            },
            "RelyingParty": "rp://api.minecraftservices.com/",
            "TokenType": "JWT",
        })
    );

    let login = &seen[2];
    assert_eq!(login.method, "POST");
    assert_eq!(login.header("content-type"), "application/json");
    assert_eq!(login.header("accept"), "application/json");
    assert_eq!(
        login.json(),
        serde_json::json!({ "identityToken": "XBL3.0 x=1234567890;invented-xsts" })
    );

    for request in &seen[3..] {
        assert_eq!(request.method, "GET");
        assert_eq!(request.header("authorization"), "Bearer invented-minecraft");
    }
}

#[tokio::test]
async fn an_xsts_refusal_ends_the_session_there() {
    let fake = Fake::start().await;
    fake.answer(XBOX_PATH, 200, &xbox_answer("invented-xbox", "1234567890"));
    fake.answer(
        XSTS_PATH,
        401,
        r#"{"Identity":"0","XErr":2148916233,"Message":""}"#,
    );
    let clock = ManualClock::new(start());
    let access = MsAccessToken::try_from(secret("invented-access")).unwrap();

    let error = fake
        .adapter(quick(), &clock)
        .minecraft_session(&access)
        .await
        .unwrap_err();

    assert_eq!(error, SessionError::Xbox(XboxRefusal::NoXboxProfile));
    assert_eq!(fake.paths(), [XBOX_PATH, XSTS_PATH]);
}

#[tokio::test]
async fn an_account_without_the_game_ends_before_the_profile() {
    let fake = Fake::start().await;
    fake.answer(XBOX_PATH, 200, &xbox_answer("invented-xbox", "1234567890"));
    fake.answer(XSTS_PATH, 200, &xbox_answer("invented-xsts", "1234567890"));
    fake.answer(LOGIN_PATH, 200, &login_answer("invented-minecraft"));
    fake.answer(
        ENTITLEMENTS_PATH,
        200,
        r#"{"items":[],"signature":"x","keyId":"1"}"#,
    );
    let clock = ManualClock::new(start());
    let access = MsAccessToken::try_from(secret("invented-access")).unwrap();

    let error = fake
        .adapter(quick(), &clock)
        .minecraft_session(&access)
        .await
        .unwrap_err();

    assert_eq!(error, SessionError::NotOwned);
    assert_eq!(
        fake.paths(),
        [XBOX_PATH, XSTS_PATH, LOGIN_PATH, ENTITLEMENTS_PATH]
    );
}

#[tokio::test]
async fn a_missing_profile_is_no_profile() {
    let fake = Fake::start().await;
    fake.answer(XBOX_PATH, 200, &xbox_answer("invented-xbox", "1234567890"));
    fake.answer(XSTS_PATH, 200, &xbox_answer("invented-xsts", "1234567890"));
    fake.answer(LOGIN_PATH, 200, &login_answer("invented-minecraft"));
    fake.answer(ENTITLEMENTS_PATH, 200, OWNED);
    fake.answer(
        PROFILE_PATH,
        404,
        r#"{"path":"/minecraft/profile","error":"NOT_FOUND","errorMessage":"x"}"#,
    );
    let clock = ManualClock::new(start());
    let access = MsAccessToken::try_from(secret("invented-access")).unwrap();

    let error = fake
        .adapter(quick(), &clock)
        .minecraft_session(&access)
        .await
        .unwrap_err();

    assert_eq!(error, SessionError::NoProfile);
}

// --- The clock -------------------------------------------------------------

#[tokio::test]
async fn a_flows_expiry_counts_from_the_clock_read_before_sending() {
    let fake = Fake::start().await;
    fake.answer(
        DEVICE_PATH,
        200,
        &device_answer("invented-device-code", "B7X4KDLQ"),
    );
    let clock = ManualClock::new(start());
    fake.advance_on_each_request(&clock, Duration::from_secs(30));

    let flow = fake
        .adapter(quick(), &clock)
        .start_device_flow()
        .await
        .unwrap();

    assert_eq!(flow.expires_at, after(900));
}

#[tokio::test]
async fn a_sessions_expiry_counts_from_the_clock_read_before_the_login() {
    let fake = Fake::start().await;
    fake.script_session(
        "invented-xbox",
        "1234567890",
        "invented-xsts",
        "invented-minecraft",
        "4f1c9a2e7b3d4e5f8a6b0c1d2e3f4a5b",
        "AfkBot1",
    );
    let clock = ManualClock::new(start());
    fake.advance_on_each_request(&clock, Duration::from_secs(10));
    let access = MsAccessToken::try_from(secret("invented-access")).unwrap();

    let session = fake
        .adapter(quick(), &clock)
        .minecraft_session(&access)
        .await
        .unwrap();

    // Xbox Live and XSTS moved the clock 20 s before the login's read.
    assert_eq!(session.expires_at, after(20 + 86_400));
}

// --- The capped reader -----------------------------------------------------

#[tokio::test]
async fn a_body_announced_over_the_cap_is_refused_before_reading() {
    let mut answer = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
        MAX_ANSWER_BYTES + 1
    )
    .into_bytes();
    answer.extend(vec![b' '; MAX_ANSWER_BYTES + 1]);
    let adapter = raw_adapter(answering(answer), "http", quick());

    let error = adapter.start_device_flow().await.unwrap_err();

    assert_eq!(
        error,
        Failure::Unexpected {
            endpoint: Endpoint::DeviceCode,
            rule: Rule::BodyTooLarge
        }
    );
}

#[tokio::test]
async fn a_body_growing_past_the_cap_is_cut_off() {
    let mut answer = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
    answer.extend(vec![b' '; MAX_ANSWER_BYTES + 1]);
    let adapter = raw_adapter(answering(answer), "http", quick());

    let error = adapter.start_device_flow().await.unwrap_err();

    assert_eq!(
        error,
        Failure::Unexpected {
            endpoint: Endpoint::DeviceCode,
            rule: Rule::BodyTooLarge
        }
    );
}

#[tokio::test]
async fn a_body_of_exactly_the_cap_is_read() {
    let mut body = device_answer("invented-device-code", "B7X4KDLQ").into_bytes();
    body.resize(MAX_ANSWER_BYTES, b' ');
    let mut answer =
        format!("HTTP/1.1 200 OK\r\nContent-Length: {MAX_ANSWER_BYTES}\r\n\r\n").into_bytes();
    answer.extend(body);
    let adapter = raw_adapter(answering(answer), "http", quick());

    let flow = adapter.start_device_flow().await.unwrap();

    assert_eq!(flow.code.expose_secret(), "invented-device-code");
}

// --- Bounds and transport --------------------------------------------------

#[tokio::test]
async fn a_request_without_an_answer_times_out() {
    let timeouts = Timeouts {
        request: Duration::from_millis(100),
        ..quick()
    };
    let adapter = raw_adapter(silent(), "http", timeouts);

    let error = within(adapter.start_device_flow()).await.unwrap_err();

    assert_eq!(error, Failure::TimedOut);
}

#[tokio::test]
async fn a_call_over_its_bound_times_out() {
    let fake = Fake::start().await;
    fake.reply(DEVICE_PATH, Reply::Hang);
    let timeouts = Timeouts {
        call: Duration::from_millis(100),
        ..quick()
    };
    let clock = ManualClock::new(start());

    let adapter = fake.adapter(timeouts, &clock);

    let error = within(adapter.start_device_flow()).await.unwrap_err();

    assert_eq!(error, Failure::TimedOut);
}

#[tokio::test]
async fn a_session_over_its_bound_times_out() {
    let fake = Fake::start().await;
    fake.answer(XBOX_PATH, 200, &xbox_answer("invented-xbox", "1234567890"));
    fake.reply(XSTS_PATH, Reply::Hang);
    let timeouts = Timeouts {
        session: Duration::from_millis(100),
        ..quick()
    };
    let clock = ManualClock::new(start());
    let access = MsAccessToken::try_from(secret("invented-access")).unwrap();

    let adapter = fake.adapter(timeouts, &clock);

    let error = within(adapter.minecraft_session(&access))
        .await
        .unwrap_err();

    assert_eq!(error, SessionError::Failed(Failure::TimedOut));
}

#[tokio::test]
async fn a_closed_port_is_unreachable() {
    let address = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let adapter = raw_adapter(address, "http", quick());

    let error = adapter.start_device_flow().await.unwrap_err();

    assert_eq!(error, Failure::Unreachable);
}

#[tokio::test]
async fn a_tls_handshake_answered_with_garbage_is_unreachable() {
    let adapter = raw_adapter(garbage(), "https", quick());

    let error = adapter.start_device_flow().await.unwrap_err();

    assert_eq!(error, Failure::Unreachable);
}

#[tokio::test]
async fn a_certificate_the_platform_doesnt_trust_is_refused() {
    let adapter = raw_adapter(untrusted_tls(), "https", quick());

    let error = adapter.start_device_flow().await.unwrap_err();

    assert_eq!(error, Failure::Certificate);
}

// --- The production client's own protections -------------------------------

#[test]
fn the_production_adapter_builds_without_network() {
    let adapter = ReqwestMicrosoftAuth::new(Arc::new(ManualClock::new(start()))).unwrap();

    assert_eq!(adapter.endpoints, Endpoints::microsoft());
    assert_eq!(adapter.timeouts, Timeouts::MICROSOFT);
    assert_eq!(format!("{adapter:?}"), "ReqwestMicrosoftAuth { .. }");
}

#[tokio::test]
async fn the_production_client_refuses_plain_http() {
    let fake = Fake::start().await;
    fake.answer(
        DEVICE_PATH,
        200,
        &device_answer("invented-device-code", "B7X4KDLQ"),
    );
    let adapter = ReqwestMicrosoftAuth::new(Arc::new(ManualClock::new(start()))).unwrap();
    let request = adapter
        .client
        .post(format!("{}{DEVICE_PATH}", fake.base()))
        .form(&[("client_id", CLIENT_ID)]);

    let result = adapter
        .exchange(Endpoint::DeviceCode, request, |answer| {
            answers::device_flow(answer, start())
        })
        .await;

    assert!(result.is_err(), "{result:?}");
    assert!(fake.seen().is_empty());
}

#[tokio::test]
async fn a_redirect_is_never_followed() {
    let fake = Fake::start().await;
    fake.reply(DEVICE_PATH, Reply::RedirectTo("/second"));
    fake.answer(
        "/second",
        200,
        &device_answer("invented-device-code", "B7X4KDLQ"),
    );
    let clock = ManualClock::new(start());

    let error = fake
        .adapter(quick(), &clock)
        .start_device_flow()
        .await
        .unwrap_err();

    assert_eq!(
        error,
        Failure::Unexpected {
            endpoint: Endpoint::DeviceCode,
            rule: Rule::Status(302)
        }
    );
    assert_eq!(fake.paths(), [DEVICE_PATH]);
}

// --- Redaction -------------------------------------------------------------

/// Every value a log must never hold, invented.
const DEVICE_CODE: &str = "afkfleet-redaction-device-code";
const USER_CODE: &str = "REDACT12";
const ACCESS: &str = "afkfleet-redaction-access-token";
const REFRESH: &str = "afkfleet-redaction-refresh-token";
const NEW_REFRESH: &str = "afkfleet-redaction-new-refresh-token";
const XBOX: &str = "afkfleet-redaction-xbox-token";
const USER_HASH: &str = "8761234598761234";
const XSTS: &str = "afkfleet-redaction-xsts-token";
const MINECRAFT: &str = "afkfleet-redaction-minecraft-token";
const NAME: &str = "AfkRedactName";
const UUID_SIMPLE: &str = "5e2d8c1b9a7f4e3d8c6b5a4f3e2d1c0b";
const UUID_HYPHENATED: &str = "5e2d8c1b-9a7f-4e3d-8c6b-5a4f3e2d1c0b";

#[tokio::test]
async fn no_secret_reaches_any_log_at_any_level() {
    log_capture::install().unwrap();
    let fake = Fake::start().await;
    let clock = ManualClock::new(start());
    let adapter = fake.adapter(quick(), &clock);
    let code = DeviceCode::try_from(secret(DEVICE_CODE)).unwrap();
    let refresh = RefreshToken::try_from(secret(REFRESH)).unwrap();
    let access = MsAccessToken::try_from(secret(ACCESS)).unwrap();

    // Successes.
    fake.answer(DEVICE_PATH, 200, &device_answer(DEVICE_CODE, USER_CODE));
    adapter.start_device_flow().await.unwrap();
    fake.answer(TOKEN_PATH, 400, &oauth_error("authorization_pending"));
    fake.answer(TOKEN_PATH, 200, &token_answer(ACCESS, REFRESH));
    adapter.poll(&code).await.unwrap();
    adapter.poll(&code).await.unwrap();
    fake.answer(TOKEN_PATH, 200, &token_answer(ACCESS, NEW_REFRESH));
    adapter.refresh(&refresh).await.unwrap();
    fake.script_session(XBOX, USER_HASH, XSTS, MINECRAFT, UUID_SIMPLE, NAME);
    adapter.minecraft_session(&access).await.unwrap();

    // Failures whose bodies or requests hold the values.
    fake.answer(DEVICE_PATH, 200, &device_answer(DEVICE_CODE, "lower-case"));
    adapter.start_device_flow().await.unwrap_err();
    fake.answer(
        TOKEN_PATH,
        200,
        &format!(r#"{{"access_token":"{ACCESS}","expires_in":3600}}"#),
    );
    adapter.poll(&code).await.unwrap_err();
    fake.answer(TOKEN_PATH, 400, &oauth_error("invalid_grant"));
    adapter.refresh(&refresh).await.unwrap_err();
    fake.answer(TOKEN_PATH, 400, &format!(r#"{{"error":"{REFRESH}"}}"#));
    adapter.refresh(&refresh).await.unwrap_err();
    fake.answer(TOKEN_PATH, 503, &token_answer(ACCESS, NEW_REFRESH));
    adapter.refresh(&refresh).await.unwrap_err();
    fake.answer(XBOX_PATH, 200, &xbox_answer(XBOX, USER_HASH));
    fake.answer(XSTS_PATH, 401, r#"{"XErr":2148916238}"#);
    adapter.minecraft_session(&access).await.unwrap_err();
    fake.answer(XBOX_PATH, 200, &xbox_answer(XBOX, USER_HASH));
    fake.answer(XSTS_PATH, 200, &xbox_answer(XSTS, USER_HASH));
    fake.answer(LOGIN_PATH, 200, &login_answer(MINECRAFT));
    fake.answer(ENTITLEMENTS_PATH, 200, OWNED);
    fake.answer(PROFILE_PATH, 200, &profile_answer(UUID_SIMPLE, "Bad Name"));
    adapter.minecraft_session(&access).await.unwrap_err();
    fake.answer(XBOX_PATH, 200, &xbox_answer(XBOX, USER_HASH));
    fake.answer(XSTS_PATH, 200, &xbox_answer(XSTS, USER_HASH));
    fake.answer(LOGIN_PATH, 200, &login_answer(MINECRAFT));
    fake.answer(ENTITLEMENTS_PATH, 200, OWNED);
    fake.answer(PROFILE_PATH, 418, &profile_answer(UUID_SIMPLE, NAME));
    adapter.minecraft_session(&access).await.unwrap_err();

    // The capture works: the failures' lines are there.
    assert!(log_capture::count("fleet_server::infra::msauth", FAILED) >= 8);
    for value in [
        DEVICE_CODE,
        USER_CODE,
        ACCESS,
        REFRESH,
        NEW_REFRESH,
        XBOX,
        USER_HASH,
        XSTS,
        MINECRAFT,
        NAME,
        UUID_SIMPLE,
        UUID_HYPHENATED,
    ] {
        log_capture::check_absent(value).unwrap();
    }
}
