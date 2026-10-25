//! The audit log's route (Plan.md P11.9, Appendix B; ADR-0020, S47, S48,
//! R64–R66): `GET /api/v1/audit`.
//!
//! - **Who:** Admins and the Owner (`ViewAuditLog`); a Member gets 403, as
//!   for the other admin lists. A plain authed route of class Standard: a
//!   read neither loses data nor widens access, so it takes no step-up
//!   (S47).
//! - **What:** every entry, newest first by ID, in keyset pages with a
//!   `SafeInt` cursor, without filters (S48); each built by
//!   [`audit_entry`] the same for every viewer (ADR-0010's P7.13): what the
//!   log stores, its names as `Open` values, the server's own entries
//!   without actor, IP or target.
//! - **Nothing else:** it reads through the read connections, writes
//!   nothing and tells the live events sockets nothing.

use axum::Json;
use axum::extract::State;
use fleet_api_types::audit::{
    AuditDetail, AuditEntryResponse, AuditPage, AuditPageQuery, AuditTargetResponse,
};
use fleet_api_types::{BoundedText, ErrorResponse, Open, SafeInt};
use fleet_core::audit::{RecordedTarget, RecordedValue};
use fleet_core::authz::{Permission, ResourceContext};

use super::timestamp;
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, ValidQuery};
use crate::http::router::AppState;
use crate::ports::audit::{AuditEntryId, AuditLimit, AuditPage as Page, AuditRecord};

/// `GET /api/v1/audit`: one page of the audit log, newest first.
#[utoipa::path(
    get,
    path = "/api/v1/audit",
    operation_id = "list_audit",
    tag = "audit",
    params(AuditPageQuery),
    responses(
        (status = 200, description = "One page of the audit log, newest first: each entry as the log stores it.", body = AuditPage),
        (status = 400, description = "A query that doesn't parse.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Only Admins and the Owner read the audit log.", body = ErrorResponse),
        (status = 422, description = "A `limit` outside 1 to 100, or a `before` below 1.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn list(
    user: AuthUser,
    State(state): State<AppState>,
    ValidQuery(query): ValidQuery<AuditPageQuery>,
) -> Result<Json<AuditPage>, ApiError> {
    user.require(Permission::ViewAuditLog, &ResourceContext::Global)?;
    let page = Page {
        before: query.before.map(|before| AuditEntryId::new(before.get())),
        // garde has checked the limit, so one out of range is a bug.
        limit: AuditLimit::new(query.limit_or_default()).map_err(ApiError::internal)?,
    };
    let listed = state.audit.page(page).await?;
    Ok(Json(AuditPage {
        items: listed
            .items
            .iter()
            .map(audit_entry)
            .collect::<Result<_, _>>()?,
        next: listed
            .next
            .map(|next| SafeInt::try_new(next.get()))
            .transpose()
            .map_err(ApiError::internal)?,
    }))
}

/// `record` as every viewer reads it: what the log stores, nothing joined,
/// so a target deleted since lists as it is (R65).
fn audit_entry(record: &AuditRecord) -> Result<AuditEntryResponse, ApiError> {
    Ok(AuditEntryResponse {
        id: SafeInt::try_new(record.id.get()).map_err(ApiError::internal)?,
        at: timestamp(record.at)?,
        actor: record.actor,
        ip: record
            .ip
            .map(|ip| BoundedText::new(&ip.to_canonical().to_string())),
        action: Open::recorded(record.action.as_str()),
        target: record.target.as_ref().map(|target| match target {
            RecordedTarget::Known(known) => AuditTargetResponse {
                kind: Open::recorded(known.kind()),
                id: BoundedText::new(&known.id().to_string()),
            },
            RecordedTarget::Unrecognized { kind, id } => AuditTargetResponse {
                kind: Open::recorded(kind.as_str()),
                id: BoundedText::new(&id.to_string()),
            },
        }),
        outcome: Open::recorded(record.outcome.as_str()),
        metadata: record
            .metadata
            .entries()
            .iter()
            .map(|(key, value)| detail(key, value))
            .collect(),
    })
}

/// One metadata value under `key`: a value only a newer server knows comes
/// as its text.
fn detail(key: &str, value: &RecordedValue) -> AuditDetail {
    let (text, int, flag) = match value {
        RecordedValue::Text(text) | RecordedValue::Unrecognized(text) => {
            (Some(BoundedText::new(text)), None, None)
        }
        // Within the safe range: the store reads anything past it as
        // unrecognized.
        RecordedValue::Int(int) => match SafeInt::try_new(*int) {
            Ok(int) => (None, Some(int), None),
            Err(_) => (Some(BoundedText::new(&int.to_string())), None, None),
        },
        RecordedValue::Bool(flag) => (None, None, Some(*flag)),
    };
    AuditDetail {
        key: BoundedText::new(key),
        text,
        int,
        flag,
    }
}
