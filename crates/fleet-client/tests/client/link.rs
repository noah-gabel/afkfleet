//! The app's link rule against the server's (ADR-0018, group F): every case
//! goes through both `fleet_client::VerifiedLink` and the server's
//! `VerificationUri`, which must agree on the verdict and on the link as
//! written back, so neither can change alone.

use fleet_api_types::accounts::VERIFICATION_URI_MAX_CHARS;
use fleet_client::VerifiedLink;
use fleet_server::ports::msauth::VerificationUri;

/// The link each rule keeps for `text`, or `None` when it refuses it.
fn verdicts(text: &str) -> (Option<String>, Option<String>) {
    let app = VerifiedLink::try_from(text)
        .ok()
        .map(|link| link.as_str().to_owned());
    let server = VerificationUri::try_from(text)
        .ok()
        .map(|link| link.as_str().to_owned());
    (app, server)
}

/// Cases with the link they must give, `None` for a refusal.
type Cases = Vec<(String, Option<String>)>;

/// `cases` as owned texts.
fn owned(cases: &[(&str, Option<&str>)]) -> Cases {
    cases
        .iter()
        .map(|(text, kept)| ((*text).to_owned(), kept.map(str::to_owned)))
        .collect()
}

/// The length rule, on the link as written back: a path that grows when
/// it's percent-encoded counts as it grew.
fn length_cases() -> Cases {
    let base = "https://www.microsoft.com/";
    let longest = format!(
        "{base}{}",
        "a".repeat(VerificationUri::MAX_LEN - base.len())
    );
    let longer = format!("{longest}a");
    // 2 characters of path that are 12 once percent-encoded.
    let growing = format!(
        "{base}{}\u{e9}\u{e9}",
        "a".repeat(VerificationUri::MAX_LEN - base.len() - 8)
    );
    let fitting = format!(
        "{base}{}\u{e9}",
        "a".repeat(VerificationUri::MAX_LEN - base.len() - 6)
    );
    let fitting_kept = fitting.replace('\u{e9}', "%C3%A9");
    vec![
        (longest.clone(), Some(longest)),
        (longer, None),
        (fitting, Some(fitting_kept)),
        (growing, None),
    ]
}

/// The server's own cases.
fn servers_cases() -> Cases {
    owned(&[
        // Accepted.
        (
            "https://www.microsoft.com/link",
            Some("https://www.microsoft.com/link"),
        ),
        (
            "https://microsoft.com/link",
            Some("https://microsoft.com/link"),
        ),
        (
            "https://WWW.MICROSOFT.COM/link",
            Some("https://www.microsoft.com/link"),
        ),
        (
            "https://www.microsoft.com:443/link",
            Some("https://www.microsoft.com/link"),
        ),
        (
            "https://www.microsoft.com",
            Some("https://www.microsoft.com/"),
        ),
        // Refused.
        ("http://www.microsoft.com/link", None),
        ("https://login.live.com/link", None),
        ("https://www.microsoft.com.example.com/link", None),
        ("https://evil.microsoft.com/link", None),
        ("https://user@www.microsoft.com/link", None),
        ("https://user:pw@www.microsoft.com/link", None),
        ("https://www.microsoft.com:8443/link", None),
        ("https://www.microsoft.com/link?otc=ABCD1234", None),
        ("https://www.microsoft.com/link?", None),
        ("https://www.microsoft.com/link#code", None),
        ("https://192.0.2.10/link", None),
        ("www.microsoft.com/link", None),
        ("", None),
    ])
}

/// What the URL parser accepts and rewrites, and what only looks alike.
fn parser_cases() -> Cases {
    owned(&[
        (
            "HTTPS://www.microsoft.com/link",
            Some("https://www.microsoft.com/link"),
        ),
        (
            "https://www.microsoft.com:0443/link",
            Some("https://www.microsoft.com/link"),
        ),
        (
            "https://www.microsoft.com:/link",
            Some("https://www.microsoft.com/link"),
        ),
        (
            "https://@www.microsoft.com/link",
            Some("https://www.microsoft.com/link"),
        ),
        (
            "https:\\\\www.microsoft.com\\link",
            Some("https://www.microsoft.com/link"),
        ),
        (
            "https://www.micro\tsoft.com/li\nnk",
            Some("https://www.microsoft.com/link"),
        ),
        (
            "  https://www.microsoft.com/link  ",
            Some("https://www.microsoft.com/link"),
        ),
        (
            "https://www.%6Dicrosoft.com/link",
            Some("https://www.microsoft.com/link"),
        ),
        (
            "https://\u{ff57}\u{ff57}\u{ff57}.microsoft.com/link",
            Some("https://www.microsoft.com/link"),
        ),
        (
            "https://www.microsoft.com/l\u{e9}nk",
            Some("https://www.microsoft.com/l%C3%A9nk"),
        ),
        // Hosts that only look alike.
        ("https://www.microsoft.com./link", None),
        ("https://www.micr\u{f8}soft.com/link", None),
        ("https://www.xn--micrsoft-v2a.com/link", None),
        ("https://www.m\u{456}crosoft.com/link", None),
        ("https://microsoft.com.evil/link", None),
        ("https://www.microsoft.com@evil.example.com/link", None),
        ("https://evil.example.com\\@www.microsoft.com/link", None),
        // Other schemes.
        ("javascript:alert(1)", None),
        ("file:///C:/path/to/afkfleet", None),
        ("ms-settings:privacy", None),
        ("wss://www.microsoft.com/link", None),
    ])
}

/// Every case with the link it must give.
fn known_cases() -> Cases {
    [servers_cases(), parser_cases(), length_cases()].concat()
}

/// Every combination of a few schemes, user parts, hosts, ports, paths,
/// queries and fragments, built in loops.
fn combinations() -> Vec<String> {
    let schemes = ["https://", "http://", "HTTPS://", "ftp://", ""];
    let users = ["", "user@", "user:pw@", "@"];
    let hosts = [
        "www.microsoft.com",
        "microsoft.com",
        "MICROSOFT.COM",
        "login.microsoftonline.com",
        "example.com",
        "192.0.2.10",
        "[2001:db8::1]",
        "www.microsoft.com.",
    ];
    let ports = ["", ":443", ":80", ":8443", ":"];
    let paths = ["", "/", "/link", "/l%69nk", "/a/b/../c"];
    let queries = ["", "?", "?otc=ABCD"];
    let fragments = ["", "#", "#x"];
    let mut all = Vec::new();
    for scheme in schemes {
        for user in users {
            for host in hosts {
                for port in ports {
                    for path in paths {
                        for query in queries {
                            for fragment in fragments {
                                all.push(format!(
                                    "{scheme}{user}{host}{port}{path}{query}{fragment}"
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    all
}

#[test]
fn the_app_and_the_server_accept_exactly_the_same_links() {
    for (text, kept) in known_cases() {
        let (app, server) = verdicts(&text);

        assert_eq!(app, server, "{text:?}: the app and the server disagree");
        assert_eq!(app, kept, "{text:?}");
    }
    let generated = combinations();
    let accepted = generated
        .iter()
        .filter(|text| {
            let (app, server) = verdicts(text);
            assert_eq!(app, server, "{text:?}: the app and the server disagree");
            app.is_some()
        })
        .count();

    // Both verdicts occur, so the agreement isn't two rules that refuse
    // everything.
    assert_eq!(generated.len(), 5 * 4 * 8 * 5 * 5 * 3 * 3);
    assert!(accepted > 0 && accepted < generated.len(), "{accepted}");
}

#[test]
fn the_app_and_the_server_share_their_hosts_and_length() {
    assert_eq!(VerifiedLink::HOSTS, VerificationUri::HOSTS);
    assert_eq!(VerifiedLink::MAX_LEN, VerificationUri::MAX_LEN);
    assert_eq!(VerifiedLink::MAX_LEN, VERIFICATION_URI_MAX_CHARS);
}
