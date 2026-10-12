//! From the Rust side's errors to the [`CommandError`] a command answers
//! (Plan.md P8.4, ADR-0017).
//!
//! fleet-client's [`ClientError`] maps variant by variant, so a new variant
//! doesn't compile until it has a code. A refusal from the server keeps its
//! status, its open code, its message, its request ID, its fields and its
//! `Retry-After`; the UI keeps a default branch for a code it doesn't know.
//! Every other error gets its code's fixed message, plus fields that name the
//! broken rule, never the value. No mapping copies a token, a URL or a body.

use fleet_api_types::FieldError;
use fleet_api_types::app::{ApiErrorDetails, CommandError, CommandErrorCode};
use fleet_client::{ApiFailure, BuildError, CaError, ClientError, ServerUrlError, StoreError};

/// The command error for a failed fleet-client call.
#[must_use]
pub(crate) fn client(error: ClientError) -> CommandError {
    let code = match error {
        ClientError::Api(failure) => return CommandError::api(details(failure)),
        ClientError::Unreachable(_) => CommandErrorCode::Unreachable,
        ClientError::Certificate(_) => CommandErrorCode::Certificate,
        ClientError::TimedOut(_) => CommandErrorCode::TimedOut,
        ClientError::TooLarge => CommandErrorCode::TooLarge,
        ClientError::UnexpectedResponse { .. } => CommandErrorCode::UnexpectedResponse,
        ClientError::NoSession => CommandErrorCode::NoSession,
        ClientError::NoPendingStep => CommandErrorCode::NoPendingStep,
        ClientError::AlreadyLoggedIn => CommandErrorCode::AlreadyLoggedIn,
        ClientError::Store(_) => CommandErrorCode::Store,
        ClientError::Interrupted(_) => CommandErrorCode::Interrupted,
    };
    CommandError::new(code)
}

/// The command error for an input that broke its rules: `invalid_input`, one
/// field per broken rule, in garde's order and path notation.
#[must_use]
pub(crate) fn invalid(report: &garde::Report) -> CommandError {
    let fields = report
        .iter()
        .map(|(path, error)| FieldError::new(&path.to_string(), error.message()))
        .collect();
    CommandError::with_fields(CommandErrorCode::InvalidInput, fields)
}

/// The command error for one input field that broke `rule`.
#[must_use]
pub(crate) fn invalid_field(path: &str, rule: &str) -> CommandError {
    CommandError::with_fields(
        CommandErrorCode::InvalidInput,
        vec![FieldError::new(path, rule)],
    )
}

/// The command error for a server address that can't be used.
#[must_use]
pub(crate) fn server_url(error: ServerUrlError) -> CommandError {
    CommandError::with_fields(
        CommandErrorCode::InvalidServerUrl,
        vec![FieldError::new("url", &error.to_string())],
    )
}

/// The command error for a custom CA that can't be used.
#[must_use]
pub(crate) fn ca(error: CaError) -> CommandError {
    CommandError::with_fields(
        CommandErrorCode::InvalidCa,
        vec![FieldError::new("ca_pem", &error.to_string())],
    )
}

/// The command error for a client reqwest couldn't build.
#[must_use]
pub(crate) fn build(_error: &BuildError) -> CommandError {
    CommandError::new(CommandErrorCode::ClientBuild)
}

/// The command error for a keychain that failed outside the client.
#[must_use]
pub(crate) fn store(_error: &StoreError) -> CommandError {
    CommandError::new(CommandErrorCode::Store)
}

/// The command error for a server profile that couldn't be written.
#[must_use]
pub(crate) fn profile_save() -> CommandError {
    CommandError::new(CommandErrorCode::ProfileSave)
}

/// The server's refusal as the UI sees it.
fn details(failure: ApiFailure) -> ApiErrorDetails {
    ApiErrorDetails {
        status: failure.status,
        code: failure.code,
        message: failure.message,
        request_id: failure.request_id,
        fields: (!failure.fields.is_empty()).then_some(failure.fields),
        retry_after_secs: failure
            .retry_after
            .map(|wait| u32::try_from(wait.as_secs()).unwrap_or(u32::MAX)),
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use fleet_api_types::{BoundedText, ErrorCode};
    use fleet_client::{StoreOp, Unexpected};
    use garde::Validate as _;
    use rstest::rstest;

    use super::*;

    fn failure(fields: Vec<FieldError>, retry_after: Option<Duration>) -> ApiFailure {
        ApiFailure {
            status: 422,
            code: ErrorCode::ValidationFailed.into(),
            message: BoundedText::new(ErrorCode::ValidationFailed.message()),
            request_id: BoundedText::new("01900000-0000-7000-8000-000000000009"),
            fields,
            retry_after,
        }
    }

    /// A `JoinError`: a task aborted before it ran.
    async fn join_error() -> tokio::task::JoinError {
        let task = tokio::spawn(core::future::pending::<()>());
        task.abort();
        task.await.unwrap_err()
    }

    #[rstest]
    #[case::unreachable(ClientError::Unreachable(None), CommandErrorCode::Unreachable)]
    #[case::certificate(ClientError::Certificate("refused".into()), CommandErrorCode::Certificate)]
    #[case::timed_out(ClientError::TimedOut(None), CommandErrorCode::TimedOut)]
    #[case::too_large(ClientError::TooLarge, CommandErrorCode::TooLarge)]
    #[case::unexpected(
        ClientError::UnexpectedResponse { status: 302, reason: Unexpected::Redirect },
        CommandErrorCode::UnexpectedResponse
    )]
    #[case::no_session(ClientError::NoSession, CommandErrorCode::NoSession)]
    #[case::no_pending_step(ClientError::NoPendingStep, CommandErrorCode::NoPendingStep)]
    #[case::already_logged_in(ClientError::AlreadyLoggedIn, CommandErrorCode::AlreadyLoggedIn)]
    #[case::store(
        ClientError::Store(StoreError::new(StoreOp::Save, "locked")),
        CommandErrorCode::Store
    )]
    fn each_client_error_has_its_code_and_fixed_message(
        #[case] error: ClientError,
        #[case] code: CommandErrorCode,
    ) {
        let mapped = client(error);

        assert_eq!(mapped, CommandError::new(code));
        assert_eq!(mapped.message.as_str(), code.message());
    }

    #[tokio::test]
    async fn an_interrupted_call_is_interrupted() {
        let mapped = client(ClientError::Interrupted(join_error().await));

        assert_eq!(mapped, CommandError::new(CommandErrorCode::Interrupted));
    }

    #[test]
    fn a_refusal_keeps_the_servers_details() {
        let fields = vec![FieldError::new("username", "length is lower than 1")];

        let mapped = client(ClientError::Api(failure(
            fields.clone(),
            Some(Duration::from_secs(30)),
        )));

        assert_eq!(
            mapped,
            CommandError::api(ApiErrorDetails {
                status: 422,
                code: ErrorCode::ValidationFailed.into(),
                message: BoundedText::new(ErrorCode::ValidationFailed.message()),
                request_id: BoundedText::new("01900000-0000-7000-8000-000000000009"),
                fields: Some(fields),
                retry_after_secs: Some(30),
            })
        );
    }

    #[test]
    fn a_refusal_without_fields_or_a_wait_leaves_both_out() {
        let mapped = client(ClientError::Api(failure(Vec::new(), None)));

        let api = mapped.api.unwrap();
        assert_eq!(api.fields, None);
        assert_eq!(api.retry_after_secs, None);
    }

    #[test]
    fn a_wait_beyond_u32_is_capped() {
        let mapped = client(ClientError::Api(failure(
            Vec::new(),
            Some(Duration::from_secs(u64::MAX)),
        )));

        assert_eq!(mapped.api.unwrap().retry_after_secs, Some(u32::MAX));
    }

    #[test]
    fn a_code_only_a_newer_server_knows_reaches_the_ui_as_it_came() {
        let unknown: fleet_api_types::Open<ErrorCode> =
            serde_json::from_value(serde_json::json!("from_a_newer_server")).unwrap();
        let refusal = ApiFailure {
            code: unknown,
            ..failure(Vec::new(), None)
        };

        let mapped = client(ClientError::Api(refusal));

        assert_eq!(mapped.api.unwrap().code.as_str(), "from_a_newer_server");
    }

    #[test]
    fn broken_rules_become_fields_naming_the_rule() {
        let request = fleet_api_types::app::ServerProfileRequest {
            url: "a".repeat(fleet_api_types::app::SERVER_URL_MAX_BYTES + 1),
            ca_pem: None,
        };
        let report = request.validate().unwrap_err();

        let mapped = invalid(&report);

        assert_eq!(mapped.code, CommandErrorCode::InvalidInput);
        let fields = mapped.fields.unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields.first().unwrap().path.as_str(), "url");
        assert!(!fields.first().unwrap().message.as_str().contains("aaaa"));
    }

    #[test]
    fn one_broken_field_names_its_path_and_rule() {
        assert_eq!(
            invalid_field("code", "length is greater than 64"),
            CommandError::with_fields(
                CommandErrorCode::InvalidInput,
                vec![FieldError::new("code", "length is greater than 64")]
            )
        );
    }

    #[test]
    fn a_refused_address_names_its_rule_under_url() {
        assert_eq!(
            server_url(ServerUrlError::PlainHttp),
            CommandError::with_fields(
                CommandErrorCode::InvalidServerUrl,
                vec![FieldError::new(
                    "url",
                    &ServerUrlError::PlainHttp.to_string()
                )]
            )
        );
    }

    #[test]
    fn a_refused_ca_names_its_rule_under_ca_pem() {
        assert_eq!(
            ca(CaError::NotACertificate { position: 2 }),
            CommandError::with_fields(
                CommandErrorCode::InvalidCa,
                vec![FieldError::new(
                    "ca_pem",
                    "PEM section 2 isn't a certificate"
                )]
            )
        );
    }

    #[test]
    fn the_app_side_failures_have_their_codes() {
        let store_error = StoreError::new(StoreOp::Clear, "locked");

        assert_eq!(
            store(&store_error),
            CommandError::new(CommandErrorCode::Store)
        );
        assert_eq!(
            profile_save(),
            CommandError::new(CommandErrorCode::ProfileSave)
        );
    }
}
