//! [`OfflineCredentials`]: session credentials in standalone mode.

use core::future::Future;

use fleet_core::bot::BotAccount;
use fleet_core::mc::{
    CredentialError, SessionCredentialProvider, SessionCredentials, SessionRequest,
};

/// The standalone agent's [`SessionCredentialProvider`] (Plan.md P4.3;
/// ADR-0013).
///
/// An offline account logs in with its name, which needs no server and no
/// token. An online account needs a Minecraft session that only the server
/// can issue (P10.6), so it's refused for good: the bot goes to
/// `Failed(SessionDenied)`. The standalone config allows offline accounts
/// only (P5.1), so that's a safety net.
#[derive(Debug, Clone, Copy, Default)]
pub struct OfflineCredentials;

impl SessionCredentialProvider for OfflineCredentials {
    fn session(
        &self,
        request: SessionRequest,
    ) -> impl Future<Output = Result<SessionCredentials, CredentialError>> + Send {
        core::future::ready(match request.account {
            BotAccount::Offline(username) => Ok(SessionCredentials::Offline { username }),
            BotAccount::Online(_) => Err(CredentialError { retryable: false }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn request(account: BotAccount, fresh: bool) -> SessionRequest {
        SessionRequest {
            bot_id: "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
            account,
            fresh,
        }
    }

    fn offline_account() -> BotAccount {
        BotAccount::Offline("AfkBot1".parse().unwrap())
    }

    fn online_account() -> BotAccount {
        BotAccount::Online("018bcfe5-6800-7cdc-8dcd-cdcdcdcdcdcd".parse().unwrap())
    }

    #[rstest]
    #[case::normal(false)]
    #[case::fresh(true)]
    #[tokio::test]
    async fn an_offline_account_logs_in_with_its_name(#[case] fresh: bool) {
        let result = OfflineCredentials
            .session(request(offline_account(), fresh))
            .await;

        let Ok(SessionCredentials::Offline { username }) = result else {
            panic!("expected offline credentials, got {result:?}");
        };
        assert_eq!(username.as_str(), "AfkBot1");
    }

    #[rstest]
    #[case::normal(false)]
    #[case::fresh(true)]
    #[tokio::test]
    async fn an_online_account_is_refused_for_good(#[case] fresh: bool) {
        let result = OfflineCredentials
            .session(request(online_account(), fresh))
            .await;

        assert_eq!(result.err(), Some(CredentialError { retryable: false }));
    }
}
