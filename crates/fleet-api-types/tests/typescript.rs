//! The TypeScript export against ts-rs's real output (Plan.md P6.9,
//! ADR-0015): which files it writes, how the DTOs look in TypeScript, and
//! that `sync` recognizes what ts-rs really writes.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use fleet_api_types::typescript::{self, TS_RS_HEADER};
use fleet_api_types::{ErrorCode, OpenEnum, SafeInt};
use serde::Serialize;
use tempfile::TempDir;
use ts_rs::{Config, TS};

/// Every file in `dir`, by name.
fn contents(dir: &Path) -> BTreeMap<String, String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            (name, fs::read_to_string(entry.path()).unwrap())
        })
        .collect()
}

/// A fresh export.
fn exported() -> (TempDir, BTreeMap<String, String>) {
    let scratch = TempDir::new().unwrap();
    typescript::export(scratch.path()).unwrap();
    let files = contents(scratch.path());
    (scratch, files)
}

/// Every file the export writes, sorted: one per type.
const EXPORTED_FILES: [&str; 121] = [
    "AccountKind.ts",
    "AccountLabel.ts",
    "AccountPage.ts",
    "AccountResponse.ts",
    "AccountRestriction.ts",
    "AccountStatus.ts",
    "AgentPage.ts",
    "AgentResponse.ts",
    "AgentStatus.ts",
    "ApiErrorDetails.ts",
    "AuditActionName.ts",
    "AuditDetail.ts",
    "AuditEntryResponse.ts",
    "AuditOutcomeName.ts",
    "AuditPage.ts",
    "AuditTargetKind.ts",
    "AuditTargetResponse.ts",
    "BotAgent.ts",
    "BotModeRequest.ts",
    "BotPage.ts",
    "BotPatchRequest.ts",
    "BotResponse.ts",
    "BotStateKind.ts",
    "BotStateReason.ts",
    "BotStatus.ts",
    "BotStatusKind.ts",
    "ChangeGrantRequest.ts",
    "ChatCommandRequest.ts",
    "ChatDirection.ts",
    "ChatFailure.ts",
    "ChatLine.ts",
    "ChatLineKind.ts",
    "ChatLineSender.ts",
    "ChatPage.ts",
    "ChatSendOutcome.ts",
    "ChatSendRequest.ts",
    "ChatSendResponse.ts",
    "CommandError.ts",
    "CommandErrorCode.ts",
    "ConnectFailure.ts",
    "CreateGrantRequest.ts",
    "CreateInviteRequest.ts",
    "CreateModeRequest.ts",
    "CreateOfflineAccountRequest.ts",
    "CreatedEnrollmentTokenResponse.ts",
    "CreatedInviteResponse.ts",
    "CreatedOfflineAccountResponse.ts",
    "CustomCaView.ts",
    "DesiredState.ts",
    "DisconnectKind.ts",
    "ErrorBody.ts",
    "ErrorCode.ts",
    "ErrorResponse.ts",
    "EventMessage.ts",
    "EventMessageKind.ts",
    "EventTicketResponse.ts",
    "FailedCodeAttempts.ts",
    "FieldError.ts",
    "GrantLevel.ts",
    "GrantPage.ts",
    "GrantResponse.ts",
    "HeldGrantPage.ts",
    "HeldGrantResponse.ts",
    "InvitePage.ts",
    "InviteResponse.ts",
    "InviteState.ts",
    "LastDisconnect.ts",
    "LinkFlowResponse.ts",
    "LinkFlowState.ts",
    "LinkStartedResponse.ts",
    "LiveDisconnect.ts",
    "LiveRefetchReason.ts",
    "LiveUpdateMessage.ts",
    "LoginMfaRequest.ts",
    "LoginRequest.ts",
    "LoginResponse.ts",
    "LoginStatus.ts",
    "LoginStepKind.ts",
    "LoginStepResponse.ts",
    "LogoutResponse.ts",
    "MeResponse.ts",
    "MfaConfirmRequest.ts",
    "MfaConfirmResponse.ts",
    "MfaEnrollResponse.ts",
    "MfaResetResponse.ts",
    "ModeAction.ts",
    "ModeActionKind.ts",
    "ModeActionRequest.ts",
    "ModeOwnership.ts",
    "ModePage.ts",
    "ModeResponse.ts",
    "ModeSchedule.ts",
    "ModeScheduleKind.ts",
    "ModeScheduleRequest.ts",
    "ModeStep.ts",
    "ModeStepRequest.ts",
    "ModeVisibility.ts",
    "PasswordChangeRequest.ts",
    "ProfileUnreadable.ts",
    "RefreshRequest.ts",
    "RefusalReason.ts",
    "RegisterRequest.ts",
    "RegisterResponse.ts",
    "SafeInt.ts",
    "ServerProfileRequest.ts",
    "ServerProfileResponse.ts",
    "ServerProfileState.ts",
    "ServerProfileView.ts",
    "SessionPage.ts",
    "SessionResponse.ts",
    "SessionServerFailure.ts",
    "SetupStepResponse.ts",
    "StepUpRequest.ts",
    "TokenResponse.ts",
    "UnplacedReason.ts",
    "UpdateModeRequest.ts",
    "UserLabel.ts",
    "UserPage.ts",
    "UserPatchRequest.ts",
    "UserResponse.ts",
    "UserRole.ts",
];

#[test]
fn the_export_writes_one_file_per_type() {
    let (_scratch, files) = exported();

    assert_eq!(files.keys().collect::<Vec<_>>(), EXPORTED_FILES);
}

/// The desktop app's IPC types (ADR-0017): never on the HTTP wire, so the
/// `OpenAPI` document leaves them out.
const APP_TYPES: [&str; 16] = [
    "ApiErrorDetails.ts",
    "CommandError.ts",
    "CommandErrorCode.ts",
    "CustomCaView.ts",
    "LiveDisconnect.ts",
    "LiveRefetchReason.ts",
    "LiveUpdateMessage.ts",
    "LoginStepKind.ts",
    "LoginStepResponse.ts",
    "LogoutResponse.ts",
    "ProfileUnreadable.ts",
    "ServerProfileRequest.ts",
    "ServerProfileResponse.ts",
    "ServerProfileState.ts",
    "ServerProfileView.ts",
    "SetupStepResponse.ts",
];

#[test]
fn the_app_export_adds_exactly_the_ipc_types_to_the_apis() {
    let api = TempDir::new().unwrap();
    let app = TempDir::new().unwrap();
    typescript::export_api(api.path()).unwrap();
    typescript::export_app(app.path()).unwrap();
    let (_all, files) = exported();
    let api = contents(api.path());
    let app = contents(app.path());

    let only_app: Vec<&String> = app.keys().filter(|name| !api.contains_key(*name)).collect();
    let combined: BTreeMap<&String, &String> = api.iter().chain(app.iter()).collect();

    assert_eq!(only_app, APP_TYPES);
    assert_eq!(combined, files.iter().collect::<BTreeMap<_, _>>());
}

#[test]
fn every_generated_file_starts_with_ts_rss_header() {
    let (_scratch, files) = exported();

    for (name, text) in &files {
        assert!(text.starts_with(TS_RS_HEADER), "{name}:\n{text}");
    }
}

#[test]
fn no_generated_file_contains_bigint() {
    let (_scratch, files) = exported();

    for (name, text) in &files {
        assert!(!text.contains("bigint"), "{name}:\n{text}");
    }
}

#[test]
fn the_error_body_types_its_fields_for_the_app() {
    let (_scratch, files) = exported();
    let body = &files["ErrorBody.ts"];

    for field in [
        "code: ErrorCode",
        "message: string",
        "request_id: string",
        "fields?: Array<FieldError>",
    ] {
        assert!(body.contains(field), "{field} in:\n{body}");
    }
}

#[test]
fn the_error_code_lists_every_code_and_warns_that_the_list_can_grow() {
    let (_scratch, files) = exported();
    let codes = &files["ErrorCode.ts"];

    for code in ErrorCode::ALL {
        let literal = format!("\"{}\"", code.as_str());
        assert!(codes.contains(&literal), "{literal} in:\n{codes}");
    }
    assert!(codes.contains("a newer server may send"), "{codes}");
}

/// A DTO with a [`SafeInt`].
#[derive(Serialize, TS)]
struct Probe {
    /// A count.
    count: SafeInt,
}

#[test]
fn a_safe_int_is_a_number_in_json_and_in_typescript() {
    let probe = Probe {
        count: SafeInt::try_new(SafeInt::MAX).unwrap(),
    };
    let dir = TempDir::new().unwrap();

    let json = serde_json::to_string(&probe).unwrap();
    Probe::export_all(&Config::new().with_out_dir(dir.path())).unwrap();

    assert_eq!(json, r#"{"count":9007199254740991}"#);
    let files = contents(dir.path());
    assert!(files["Probe.ts"].contains("count: SafeInt"), "{files:?}");
    assert!(
        files["SafeInt.ts"].contains("export type SafeInt = number;"),
        "{files:?}"
    );
    assert!(
        files.values().all(|text| !text.contains("bigint")),
        "{files:?}"
    );
}

#[test]
fn syncing_over_a_previous_real_export_replaces_it() {
    let dir = TempDir::new().unwrap();
    typescript::export(dir.path()).unwrap();
    // A stale type ts-rs really generated, which the new export no longer has.
    Probe::export_all(&Config::new().with_out_dir(dir.path())).unwrap();
    assert!(dir.path().join("Probe.ts").exists());
    let (scratch, fresh) = exported();

    typescript::sync(scratch.path(), dir.path()).unwrap();

    assert_eq!(contents(dir.path()), fresh);
    assert_eq!(typescript::compare(scratch.path(), dir.path()).unwrap(), []);
}
