//! What every answer on the gRPC port shares (ADR-0019, S14, S24): the
//! call's request ID in the metadata, a fixed message per case, and the
//! names log lines give gRPC's codes.

use fleet_core::id::RequestId;
use tokio::time::Instant;
use tonic::metadata::{AsciiMetadataValue, MetadataMap};
use tonic::{Code, Status};

/// The metadata every answer carries: the call's request ID.
pub const REQUEST_ID: &str = fleet_proto::enrollment::REQUEST_ID;
/// The metadata of an answer that asks the client to wait: whole seconds.
pub const RETRY_AFTER: &str = "retry-after";

/// A refusal with `code` and `message`, carrying the call's request ID and,
/// if given, `retry-after` in whole seconds.
pub(crate) fn refusal(
    code: Code,
    message: &'static str,
    id: RequestId,
    retry_after: Option<u64>,
) -> Status {
    let mut metadata = MetadataMap::new();
    request_id(&mut metadata, id);
    if let Some(seconds) = retry_after {
        metadata.insert(RETRY_AFTER, AsciiMetadataValue::from(seconds));
    }
    Status::with_metadata(code, message, metadata)
}

/// Puts call `id`'s request ID into `metadata`.
pub(crate) fn request_id(metadata: &mut MetadataMap, id: RequestId) {
    if let Ok(value) = AsciiMetadataValue::try_from(id.to_string()) {
        metadata.insert(REQUEST_ID, value);
    }
}

/// The milliseconds since `started`.
pub(crate) fn latency_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// `code`'s name in a log line, as gRPC spells it.
pub(crate) const fn code_name(code: Code) -> &'static str {
    match code {
        Code::Ok => "ok",
        Code::Cancelled => "cancelled",
        Code::Unknown => "unknown",
        Code::InvalidArgument => "invalid_argument",
        Code::DeadlineExceeded => "deadline_exceeded",
        Code::NotFound => "not_found",
        Code::AlreadyExists => "already_exists",
        Code::PermissionDenied => "permission_denied",
        Code::ResourceExhausted => "resource_exhausted",
        Code::FailedPrecondition => "failed_precondition",
        Code::Aborted => "aborted",
        Code::OutOfRange => "out_of_range",
        Code::Unimplemented => "unimplemented",
        Code::Internal => "internal",
        Code::Unavailable => "unavailable",
        Code::DataLoss => "data_loss",
        Code::Unauthenticated => "unauthenticated",
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;

    #[test]
    fn every_code_has_its_grpc_name() {
        assert_eq!(code_name(Code::Ok), "ok");
        assert_eq!(code_name(Code::ResourceExhausted), "resource_exhausted");
        assert_eq!(code_name(Code::Unauthenticated), "unauthenticated");
        assert_eq!(code_name(Code::DeadlineExceeded), "deadline_exceeded");
        assert_eq!(code_name(Code::FailedPrecondition), "failed_precondition");
        assert_eq!(code_name(Code::OutOfRange), "out_of_range");
    }

    #[test]
    fn a_refusal_carries_its_request_id_and_wait() {
        let id = RequestId::new_v7(DateTime::UNIX_EPOCH, [7; 10]).unwrap();

        let status = refusal(Code::Unavailable, "busy", id, Some(1));

        assert_eq!(status.code(), Code::Unavailable);
        assert_eq!(status.message(), "busy");
        assert_eq!(
            status.metadata().get(REQUEST_ID).unwrap().to_str().unwrap(),
            id.to_string()
        );
        assert_eq!(
            status
                .metadata()
                .get(RETRY_AFTER)
                .unwrap()
                .to_str()
                .unwrap(),
            "1"
        );
    }
}
