//! [`AuditAction`]: what an audit entry records, as a stable dotted name.

use core::fmt;
use std::borrow::Cow;

use super::AuditError;
use super::name::{self, Dots};

/// What happened, as a dotted name like `security.refresh_token_reuse`:
/// two or more parts of `a`–`z`, `0`–`9` and `_`, separated by dots, at most
/// [`AuditAction::MAX_LEN`] characters.
///
/// Code that records an entry uses only the constants each phase lists in
/// one `audit_actions!` block, and [`AuditAction::ALL`] holds them all, so
/// one test checks that every action is valid and that no two share a name.
///
/// # Naming (ADR-0016)
/// - **Actions someone performs** are `area.verb` in the imperative:
///   `user.create`, `auth.login`, `user.set_role`, `invite.create`,
///   `invite.use`.
/// - **Things the server detects** are `security.<what>` as a noun:
///   `security.refresh_token_reuse`.
/// - **Success or failure is the outcome column,** never part of the name.
/// - **Details** such as `via` and `role` go into the metadata. Their values
///   are names from closed enums, or typed values the server made (IDs,
///   timestamps), never text a client sent.
/// - **A merged name is never renamed or dropped:** it's stored in rows
///   forever. A snapshot of every name makes any change show in review.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuditAction(Cow<'static, str>);

impl AuditAction {
    /// The longest name, in characters.
    pub const MAX_LEN: usize = 64;

    /// Returns the stable name stored in the `action` column.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Defines the listed actions as constants, and [`AuditAction::ALL`]. The
/// names aren't checked here: the list's test checks every one.
macro_rules! audit_actions {
    ($($(#[$doc:meta])* $constant:ident = $name:literal;)*) => {
        impl AuditAction {
            $(
                $(#[$doc])*
                pub const $constant: Self = Self(Cow::Borrowed($name));
            )*

            /// Every listed action, for the test that checks them all.
            pub const ALL: &'static [Self] = &[$(Self::$constant),*];
        }
    };
}

audit_actions! {
    /// A user came to exist: the Owner through `user create-owner`
    /// (`via` = `cli`; no actor or IP), or a registration through an invite
    /// (`via` = `invite`; actor the new user, the client's IP, metadata
    /// `invite_id` too, next to the invite's `invite.use`). Target: the new
    /// user. Metadata: `role` and `via`. Failure: a registration with a real
    /// invite that can't be used, a sign the code leaked: no actor, the
    /// client's IP, target the invite, metadata `reason` (`used`,
    /// `revoked`, `expired` or `creator_unauthorized`). An unknown code is
    /// never recorded.
    USER_CREATE = "user.create";

    /// A refresh token that its session had already replaced was presented
    /// again while the session was live: someone else holds a copy, so the
    /// whole session (the token family) is revoked in the same commit
    /// (§7.2). Outcome: Failure. No actor (whoever presented it isn't
    /// authenticated); the presenting client's IP. Target: the session's
    /// user. Metadata: `session_id` (the revoked session) and `rotated_at`
    /// (when the presented token was replaced).
    SECURITY_REFRESH_TOKEN_REUSE = "security.refresh_token_reuse";

    /// A failed login started a lock of its username (§7.4): the threshold's
    /// failure, and each one after it, whose lock doubles up to an hour.
    /// Recorded in the failing attempt's own transaction. Outcome: Failure.
    /// No actor; the client's IP. Target: the user, when the name is a
    /// user's; none otherwise, since the typed name is never stored.
    /// Metadata: `failures` (the streak's count) and `locked_until`.
    SECURITY_LOCKOUT = "security.lockout";

    /// A user without 2FA confirmed their first TOTP authenticator with a
    /// setup token from the password step (§7.2): its secret and 10
    /// recovery codes are stored. Outcome: Success. Actor and target: the
    /// user; the client's IP.
    MFA_ENROLL = "mfa.enroll";

    /// A user replaced their TOTP authenticator: started with step-up, it's
    /// confirmed with the new authenticator's code, which swaps the secret
    /// and the recovery codes and revokes every other session of the user
    /// in the same commit. Outcome: Success. Actor and target: the user; the
    /// client's IP. Metadata: `sessions_revoked`.
    MFA_RESET = "mfa.reset";

    /// A login (§7.2). Success when its code step issues the session's
    /// tokens: actor and target the user, the client's IP, metadata
    /// `session_id`, `method` (`totp` or `recovery_code`), `login_step_id`
    /// (since Phase 8, group E) and, only when the stored password hash was
    /// re-hashed with the current parameters, `rehashed`. Failure for every
    /// counted refusal, in the lockout's commit, and for a code step refused
    /// because the password or the authenticator changed meanwhile: no
    /// actor, the client's IP, target the user (none for an unknown name,
    /// which is never stored), metadata `step` (`password` or `code`) and
    /// `reason` (`wrong_password`, `wrong_code`, `disabled`,
    /// `password_changed` or `mfa_changed`); a code step's failure also
    /// `login_step_id`. The login step's ID ties the code step's entries to
    /// the password step that issued its mfa token, so `GET /me` leaves the
    /// typos of the login that succeeded out of its count.
    AUTH_LOGIN = "auth.login";

    /// A user ended their session. Outcome: Success. Actor: the user; target:
    /// the session; the client's IP.
    AUTH_LOGOUT = "auth.logout";

    /// A user entered their password again, which allows step-up actions in
    /// this session for 5 minutes (§7.2). Actor: the user; target: the
    /// session; the client's IP. Failure: metadata `reason`
    /// (`wrong_password`, counted toward the lockout, or `password_changed`).
    AUTH_STEP_UP = "auth.step_up";

    /// The Owner changed a user's role, between Member and Admin (ADR-0010).
    /// Every session of the user is revoked in the same commit, and a
    /// demotion to Member revokes their pending invites too (one
    /// `invite.revoke` each). Outcome: Success. Actor: the Owner; target:
    /// the user; the client's IP. Metadata: `role` (the new one),
    /// `previous_role` and `sessions_revoked`.
    USER_SET_ROLE = "user.set_role";

    /// A user was disabled or enabled: by the Owner, or by an Admin for a
    /// Member (ADR-0010). Every session of the user is revoked in the same
    /// commit, and disabling revokes their pending invites too (one
    /// `invite.revoke` each). Outcome: Success. Actor: who changed it;
    /// target: the user; the client's IP. Metadata: `disabled` and
    /// `sessions_revoked`.
    USER_SET_DISABLED = "user.set_disabled";

    /// An Admin or the Owner created an invite to register: a Member invite,
    /// or an Admin invite by the Owner. Outcome: Success. Actor: its
    /// creator; target: the invite; the client's IP. Metadata: `role` and
    /// `expires_at`.
    INVITE_CREATE = "invite.create";

    /// A pending invite was revoked: through `DELETE /invites/{id}`, or
    /// because its creator was disabled or demoted to Member, in that
    /// change's commit. Outcome: Success. Actor: who revoked it, or who made
    /// the change; target: the invite; the client's IP. Metadata: `role`
    /// and, when the creator's change revoked it, `reason`
    /// (`creator_disabled` or `creator_demoted`).
    INVITE_REVOKE = "invite.revoke";

    /// An invite registered a user, in the same commit as their
    /// `user.create`. Outcome: Success. Actor: the new user; target: the
    /// invite; the client's IP. Metadata: `user_id` and `role`.
    INVITE_USE = "invite.use";

    /// A user changed their password, after stepping up. Every other session
    /// of the user is revoked in the same commit, and this session's step-up
    /// ends. Outcome: Success. Actor and target: the user; the client's IP.
    /// Metadata: `sessions_revoked`.
    USER_CHANGE_PASSWORD = "user.change_password";

    /// A user ended another of their own sessions (`DELETE
    /// /me/sessions/{id}`; their current one ends with `auth.logout`).
    /// Outcome: Success. Actor: the user; target: the session; the client's
    /// IP.
    SESSION_REVOKE = "session.revoke";

    /// `afkfleet-server vault rotate-key` ran on the server's host (P9.2,
    /// ADR-0018). No actor and no IP: an operator ran it from the command
    /// line.
    /// - **Success:** every secret sealed under another key than the active
    ///   one was re-encrypted under the active key, in this one commit. No
    ///   target (it's the whole vault). Metadata: `sealed_with` (the active
    ///   key's ID), `totp` and `ms_refresh` (how many TOTP secrets and
    ///   Microsoft refresh tokens were re-encrypted; a metadata key never
    ///   holds a secret word). A run that finds nothing to re-encrypt records
    ///   nothing.
    /// - **Failure:** one entry per secret that didn't open, all in one
    ///   commit, and nothing was re-encrypted. A secret that doesn't open
    ///   may mean someone changed the database. Target: the user (a TOTP
    ///   secret) or the account (a Microsoft refresh token). Metadata:
    ///   `reason` (`unknown_key`, `rejected` or `malformed`) and, when the
    ///   envelope's header is readable, `sealed_with` (the key it names).
    VAULT_ROTATE_KEY = "vault.rotate_key";

    /// The session-token service (P9.5, ADR-0018) stored an account's
    /// refresh token after a refresh, in the commit that stores it. No actor
    /// or IP: the server refreshed it to start a Minecraft session. Outcome:
    /// Success. Target: the account. Metadata: `rotated` (whether Microsoft
    /// sent a new token, or the one sent is kept).
    ACCOUNT_REFRESH = "account.refresh";

    /// An account's status changed. From the session-token service (P9.5,
    /// ADR-0018): no actor or IP, the server decided. Outcome: Success.
    /// Target: the account. Metadata: `status` (the new one),
    /// `previous_status` and `reason`: `invalid_grant` when Microsoft refused
    /// the refresh token (`reauth_required`), or the refusal's reason when
    /// Minecraft's side refused the account (`refused`; `not_owned`,
    /// `no_profile`, `xbox_*`), with `xerr` (Xbox Live's code) for
    /// `xbox_other` only.
    ACCOUNT_SET_STATUS = "account.set_status";

    /// The session-token service (P9.5, ADR-0018) found that the account's
    /// Minecraft name changed, and stored the profile's current one. No actor
    /// or IP. Outcome: Success. Target: the account. Metadata: `name` and
    /// `previous_name`.
    ACCOUNT_RENAME = "account.rename";

    /// The session-token service (P9.5, ADR-0018) started a Minecraft session
    /// for an account whose profile has another UUID than the one linked, so
    /// it handed out nothing and refused the account (`refused` with the
    /// reason `profile_mismatch`) in the same commit. Outcome: Failure. No
    /// actor or IP. Target: the account. Metadata: `status` and
    /// `previous_status`; never either UUID.
    SECURITY_ACCOUNT_PROFILE_MISMATCH = "security.account_profile_mismatch";

    /// A user started a Microsoft link flow (P9.4, ADR-0018): Microsoft
    /// answered with a device code, and the start passed every cap. Only
    /// accepted starts are recorded, and the daily cap counts these entries.
    /// Outcome: Success. Target: the user. Metadata: `flow` (the flow's ID),
    /// and `account` (its ID) when it links an account again (P9.6), whose
    /// starts the daily cap counts alike.
    ACCOUNT_START_LINK = "account.start_link";

    /// A user linked a Minecraft account (P9.4, P9.7, ADR-0018), in the commit
    /// that stores it. Success: target the new account; metadata `kind`,
    /// `name`, `status`, and for a Microsoft account `uuid` and `flow`.
    /// Failure (nothing stored): target the user; metadata `reason` (`taken`:
    /// the Minecraft account is already linked, or `over_quota`), `kind`,
    /// `name`, and for a Microsoft account `uuid` and `flow`.
    ACCOUNT_LINK = "account.link";

    /// An Admin or the Owner approved a pending link (P9.6, ADR-0018): the
    /// account is active from this commit on. Outcome: Success. Target: the
    /// account. Metadata: `owner` (the linker's ID), `name` and `uuid`.
    ACCOUNT_APPROVE = "account.approve";

    /// An Admin or the Owner refused a pending link (P9.6, ADR-0018), which
    /// deletes the account, its refresh token and its grants in this commit.
    /// Outcome: Success. Target: the account, which is gone. Metadata:
    /// `owner` (the linker's ID), `name` and `uuid`.
    ACCOUNT_REFUSE = "account.refuse";

    /// A user deleted an account (P9.6), with its refresh token and its
    /// grants, in this commit. Outcome: Success. Target: the account, which
    /// is gone. Metadata: `owner` (its owner's ID), `kind`, `name`, `status`,
    /// and for a Microsoft account `uuid`.
    ACCOUNT_DELETE = "account.delete";

    /// A user linked a Microsoft account again (P9.6, ADR-0018). Success, in
    /// the commit that stores its new refresh token and sets it active:
    /// metadata `flow`, `previous_status`, `name` (the profile's current
    /// one) and, when it changed, `previous_name`. Failure (nothing stored):
    /// the profile has another UUID than the account's; metadata `reason`
    /// (`profile_mismatch`) and `flow`, never either UUID. Target: the
    /// account.
    ACCOUNT_RELINK = "account.relink";

    /// A user gave another user a grant on an account (P9.6, ADR-0018).
    /// Success: metadata `grantee` (their ID) and `level`. Failure (nothing
    /// stored): the name typed can't be granted, which the user was told
    /// with the same answer for every such name; metadata `reason`
    /// (`unknown_user`, `disabled`, or `implicit_manage`: the account's
    /// owner, the Owner, or an Admin on a Member's account), never the
    /// name. Target: the account.
    GRANT_CREATE = "grant.create";

    /// A user changed another user's grant on an account (P9.6). Outcome:
    /// Success. Target: the account. Metadata: `grantee` (their ID), `level`
    /// and `previous_level`.
    GRANT_SET_LEVEL = "grant.set_level";

    /// A user revoked another user's grant on an account (P9.6). Outcome:
    /// Success. Target: the account. Metadata: `grantee` (their ID) and
    /// `level` (the one revoked).
    GRANT_REVOKE = "grant.revoke";

    /// An enrollment token was created for one new agent (P10.3, ADR-0019):
    /// by the Owner through `POST /agents/enrollment-tokens` after stepping
    /// up (actor: the Owner; the client's IP), or on the server's host with
    /// `afkfleet-server agent create-enrollment-token` (no actor, no IP).
    /// Outcome: Success. Target: the token. Metadata: `expires_at` and `via`
    /// (`http` or `cli`); never the token or its hash.
    ENROLLMENT_TOKEN_CREATE = "enrollment_token.create";

    /// An enrollment token enrolled an agent (P10.3), in the commit that
    /// creates the agent, with its `agent.enroll`. Outcome: Success. No
    /// actor; the agent's address. Target: the token. Metadata: `agent` (the
    /// new agent's ID).
    ENROLLMENT_TOKEN_USE = "enrollment_token.use";

    /// An agent enrolled with a token (P10.3, ADR-0019). No actor: whoever
    /// holds a token may enroll; the agent's address. Success, in the commit
    /// that creates the agent and stores its certificate's fingerprint:
    /// target the agent; metadata `fingerprint`, `not_after`, `name` and
    /// `enrollment` (the token's ID). Failure, nothing stored: a real token
    /// that expired or was used already, at most 10 entries per token;
    /// target the token; metadata `reason` (`expired` or `used`) and, for a
    /// used one, `agent` (the agent it enrolled). An unknown token is never
    /// recorded.
    AGENT_ENROLL = "agent.enroll";

    /// An agent connected (P10.4, ADR-0019, S29, S30). No actor; the agent's
    /// address; target the agent. Success, in the transaction that records
    /// its hello's capacity and version before `Welcome`: metadata
    /// `fingerprint` (the certificate it connected with), `version` and
    /// `capacity`. Failure, nothing changed: a certificate the server knows
    /// was refused; metadata `reason` (`revoked`, `expired`, `disabled`,
    /// `san_mismatch` or `name_mismatch`) and `fingerprint`, at most 10 per
    /// agent in any 24 hours. A certificate no agent holds is never recorded.
    AGENT_CONNECT = "agent.connect";

    /// An agent's stream ended (P10.4, ADR-0019, S29). Outcome: Success. No
    /// actor; the agent's address; target the agent. Metadata: `reason`
    /// (`closed`, `heartbeat_timeout`, `stream_error`, `protocol_error`,
    /// `too_slow`, `disabled`, `certificate_revoked`, `certificate_expired`
    /// or `server_shutdown`) and `connected_at`.
    AGENT_DISCONNECT = "agent.disconnect";

    /// Two processes hold one agent's identity (P10.5, ADR-0019, S26): a
    /// message arrived on the agent's live stream after a second connection
    /// with one of its certificates was refused. Outcome: Failure. No actor;
    /// the refused connection's address; target the agent. Metadata:
    /// `fingerprint` (the refused connection's certificate), `live_ip` (the
    /// live stream's address, when known), `connected_at` (when the live
    /// stream's hello was accepted) and `refused_at`. At most once an hour
    /// per agent.
    SECURITY_AGENT_IDENTITY_CONFLICT = "security.agent_identity_conflict";

    /// An Admin or the Owner, or the server's host, disabled an agent
    /// (P10.10, ADR-0019, S31): `agents.enabled` off and every unrevoked
    /// certificate revoked, in one transaction, then its live stream ended.
    /// Outcome: Success. Actor and IP: the user's (`via` = `http`), or none
    /// from the host (`via` = `cli`). Target: the agent. Metadata: `name`,
    /// `certificates_revoked` and `via`. An agent disabled already is no
    /// change, and no entry.
    AGENT_DISABLE = "agent.disable";
}

/// For reading rows back. Code that records an entry uses the listed
/// constants instead.
impl TryFrom<&str> for AuditAction {
    type Error = AuditError;

    fn try_from(text: &str) -> Result<Self, AuditError> {
        name::check(text, Self::MAX_LEN, Dots::Required)?;
        Ok(Self(Cow::Owned(text.to_owned())))
    }
}

impl fmt::Display for AuditAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use rstest::rstest;

    use super::*;

    /// Why a list of actions isn't acceptable: an invalid name, or two
    /// actions with the same name, which would make the log ambiguous.
    fn list_problems(list: &[AuditAction]) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut problems = Vec::new();
        for action in list {
            if let Err(error) = AuditAction::try_from(action.as_str()) {
                problems.push(format!("{}: {error}", action.as_str()));
            }
            if !seen.insert(action.as_str()) {
                problems.push(format!("{}: listed twice", action.as_str()));
            }
        }
        problems
    }

    /// Names are stored in rows forever, so the snapshot makes any rename or
    /// removal show in review (ADR-0016).
    #[test]
    fn every_name_is_kept() {
        let names: Vec<&str> = AuditAction::ALL.iter().map(AuditAction::as_str).collect();

        insta::assert_json_snapshot!(names);
    }

    #[test]
    fn every_listed_action_is_valid_and_unique() {
        assert_eq!(list_problems(AuditAction::ALL), Vec::<String>::new());
    }

    #[test]
    fn the_list_check_finds_invalid_and_duplicate_names() {
        let list = [
            AuditAction(Cow::Borrowed("user.create")),
            AuditAction(Cow::Borrowed("User.Create")),
            AuditAction(Cow::Borrowed("user.create")),
        ];

        assert_eq!(
            list_problems(&list),
            [
                "User.Create: a name may only use a-z, 0-9 and _; character 0 isn't one of them",
                "user.create: listed twice",
            ]
        );
    }

    #[rstest]
    #[case::two_parts("user.create")]
    #[case::three_parts("security.refresh_token.reuse")]
    #[case::digits_and_underscores("p7_13.login_2fa")]
    fn valid_actions_read_back(#[case] text: &str) {
        let action = AuditAction::try_from(text).unwrap();

        assert_eq!(action.as_str(), text);
        assert_eq!(action.to_string(), text);
    }

    #[test]
    fn the_length_limit_is_64_characters() {
        let longest = format!("a.{}", "b".repeat(62));
        let too_long = format!("a.{}", "b".repeat(63));

        assert_eq!(
            AuditAction::try_from(longest.as_str()).unwrap().as_str(),
            longest
        );
        assert_eq!(
            AuditAction::try_from(too_long.as_str()),
            Err(AuditError::NameLength { len: 65, max: 64 })
        );
    }

    #[rstest]
    #[case::empty("", AuditError::NameLength { len: 0, max: 64 })]
    #[case::one_part("login", AuditError::ActionShape)]
    #[case::empty_part("user..create", AuditError::ActionShape)]
    #[case::leading_dot(".user", AuditError::ActionShape)]
    #[case::trailing_dot("user.", AuditError::ActionShape)]
    #[case::uppercase("user.Create", AuditError::NameChar { index: 5 })]
    #[case::space("user create", AuditError::NameChar { index: 4 })]
    #[case::dash("user.re-create", AuditError::NameChar { index: 7 })]
    #[case::non_ascii("user.créate", AuditError::NameChar { index: 7 })]
    fn invalid_actions_are_refused(#[case] text: &str, #[case] expected: AuditError) {
        assert_eq!(AuditAction::try_from(text), Err(expected));
    }
}
