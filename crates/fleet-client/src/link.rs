//! [`VerifiedLink`]: the link to Microsoft's sign-in page that a link flow's
//! start answers with, checked by the app before it opens it (Plan.md P9.8,
//! ADR-0018).
//!
//! The server checks the link it hands out with group B's rule
//! (`fleet_server::ports::msauth::VerificationUri`). The app checks it again
//! with the same rule and the same parser, reqwest's `Url`, so a server that
//! sends any other link can't make the app open it: https only, the host
//! exactly one of [`VerifiedLink::HOSTS`], no user, no port other than 443,
//! no query or fragment, and at most [`VerifiedLink::MAX_LEN`] characters as
//! written back. A test runs every case through both rules, so they can't
//! drift apart.

use reqwest::Url;
use thiserror::Error;

/// A link to Microsoft's sign-in page that passed the rule; see the
/// [module docs](self). Only [`VerifiedLink::try_from`] builds one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedLink(String);

/// Why a text isn't a [`VerifiedLink`]. It doesn't carry the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error(
    "a verification link is https to www.microsoft.com or microsoft.com, without a user, another port, a query or a fragment, at most 256 characters"
)]
pub struct VerifiedLinkError;

impl VerifiedLink {
    /// The hosts the link may name.
    pub const HOSTS: [&str; 2] = ["www.microsoft.com", "microsoft.com"];

    /// The longest link, in characters (it's ASCII once parsed).
    pub const MAX_LEN: usize = 256;

    /// The link, as parsed and written back.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for VerifiedLink {
    type Error = VerifiedLinkError;

    fn try_from(text: &str) -> Result<Self, VerifiedLinkError> {
        let url = Url::parse(text).map_err(|_| VerifiedLinkError)?;
        let allowed = url.scheme() == "https"
            && url
                .host_str()
                .is_some_and(|host| Self::HOSTS.contains(&host))
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.as_str().len() <= Self::MAX_LEN;
        if allowed {
            Ok(Self(url.as_str().to_owned()))
        } else {
            Err(VerifiedLinkError)
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::www("https://www.microsoft.com/link", "https://www.microsoft.com/link")]
    #[case::bare_host("https://microsoft.com/link", "https://microsoft.com/link")]
    #[case::upper_case_host("https://WWW.MICROSOFT.COM/link", "https://www.microsoft.com/link")]
    #[case::explicit_default_port(
        "https://www.microsoft.com:443/link",
        "https://www.microsoft.com/link"
    )]
    #[case::root("https://www.microsoft.com", "https://www.microsoft.com/")]
    fn an_https_link_on_an_allowed_host_is_accepted_as_written_back(
        #[case] text: &str,
        #[case] kept: &str,
    ) {
        assert_eq!(VerifiedLink::try_from(text).unwrap().as_str(), kept);
    }

    #[rstest]
    #[case::http("http://www.microsoft.com/link")]
    #[case::other_host("https://login.live.com/link")]
    #[case::lookalike_host("https://www.microsoft.com.example.com/link")]
    #[case::subdomain("https://evil.microsoft.com/link")]
    #[case::user("https://user@www.microsoft.com/link")]
    #[case::user_and_password("https://user:pw@www.microsoft.com/link")]
    #[case::other_port("https://www.microsoft.com:8443/link")]
    #[case::query("https://www.microsoft.com/link?otc=ABCD1234")]
    #[case::empty_query("https://www.microsoft.com/link?")]
    #[case::fragment("https://www.microsoft.com/link#code")]
    #[case::ip("https://192.0.2.10/link")]
    #[case::not_a_url("www.microsoft.com/link")]
    #[case::javascript("javascript:alert(1)")]
    #[case::file("file:///C:/path/to/afkfleet")]
    #[case::empty("")]
    fn any_other_link_is_refused(#[case] text: &str) {
        assert_eq!(VerifiedLink::try_from(text), Err(VerifiedLinkError));
    }

    #[test]
    fn the_error_names_the_rule_and_never_the_link() {
        assert_eq!(
            VerifiedLinkError.to_string(),
            "a verification link is https to www.microsoft.com or microsoft.com, without a user, another port, a query or a fragment, at most 256 characters"
        );
    }

    /// An allowed link of exactly `len` characters.
    fn link_of(len: usize) -> String {
        let base = "https://www.microsoft.com/";
        format!("{base}{}", "a".repeat(len - base.len()))
    }

    #[test]
    fn a_link_of_256_characters_is_accepted_and_a_longer_one_refused() {
        let longest = link_of(VerifiedLink::MAX_LEN);
        let longer = link_of(VerifiedLink::MAX_LEN + 1);

        assert_eq!(VerifiedLink::MAX_LEN, 256);
        assert_eq!(
            VerifiedLink::try_from(longest.as_str()).unwrap().as_str(),
            longest
        );
        assert_eq!(
            VerifiedLink::try_from(longer.as_str()),
            Err(VerifiedLinkError)
        );
    }

    #[test]
    fn the_length_is_the_written_back_links() {
        // The text sent is 4 characters longer (`:443`); the link kept is
        // exactly the longest.
        let host = "https://www.microsoft.com";
        let path = format!("/{}", "a".repeat(VerifiedLink::MAX_LEN - host.len() - 1));

        assert_eq!(
            VerifiedLink::try_from(format!("{host}:443{path}").as_str())
                .unwrap()
                .as_str()
                .len(),
            VerifiedLink::MAX_LEN
        );
        assert_eq!(
            VerifiedLink::try_from(format!("{host}:443{path}a").as_str()),
            Err(VerifiedLinkError)
        );
    }
}
