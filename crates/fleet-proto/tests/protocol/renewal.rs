//! A certificate renewal's messages (Plan.md P13.6; ADR-0027): the answer
//! crosses the wire unchanged, every broken field is refused with its
//! error, the request stays empty, and the limits hold together.

use chrono::{DateTime, Utc};
use fleet_proto::control::{
    MAX_AGENT_MESSAGE_BYTES, MAX_SERVER_MESSAGE_BYTES, RENEW_CLIENT_DEADLINE,
    RENEW_SERVER_DEADLINE, RenewResponse,
};
use fleet_proto::enrollment::MAX_CERTIFICATE_BYTES;
use fleet_proto::generated as wire;
use fleet_proto::{ConvertError, Problem};
use prost::Message as _;
use rstest::rstest;

const AT: i64 = 1_790_000_000_000;

fn response() -> RenewResponse {
    RenewResponse {
        certificate_pem: "C".repeat(400),
        not_after: DateTime::from_timestamp_millis(AT).unwrap(),
    }
}

fn wire_response() -> wire::RenewResponse {
    wire::RenewResponse {
        certificate_pem: "C".repeat(400),
        not_after_unix_ms: Some(AT),
    }
}

#[test]
fn a_response_crosses_the_wire_unchanged() {
    let bytes = wire::RenewResponse::try_from(response())
        .unwrap()
        .encode_to_vec();

    let again =
        RenewResponse::try_from(wire::RenewResponse::decode(bytes.as_slice()).unwrap()).unwrap();

    assert_eq!(again, response());
}

#[rstest]
#[case::no_certificate(
    wire::RenewResponse { certificate_pem: String::new(), ..wire_response() },
    ConvertError::new("RenewResponse.certificate_pem", Problem::Missing)
)]
#[case::certificate_too_large(
    wire::RenewResponse { certificate_pem: "C".repeat(MAX_CERTIFICATE_BYTES + 1), ..wire_response() },
    ConvertError::new("RenewResponse.certificate_pem", Problem::OutOfRange)
)]
#[case::no_expiry(
    wire::RenewResponse { not_after_unix_ms: None, ..wire_response() },
    ConvertError::new("RenewResponse.not_after_unix_ms", Problem::Missing)
)]
#[case::expiry_out_of_range(
    wire::RenewResponse { not_after_unix_ms: Some(i64::MAX), ..wire_response() },
    ConvertError::new("RenewResponse.not_after_unix_ms", Problem::OutOfRange)
)]
fn a_broken_response_is_refused(
    #[case] response: wire::RenewResponse,
    #[case] error: ConvertError,
) {
    assert_eq!(RenewResponse::try_from(response), Err(error));
}

#[test]
fn the_largest_certificate_is_accepted() {
    let largest = wire::RenewResponse {
        certificate_pem: "C".repeat(MAX_CERTIFICATE_BYTES),
        ..wire_response()
    };

    let read = RenewResponse::try_from(largest).unwrap();

    assert_eq!(read.certificate_pem.len(), MAX_CERTIFICATE_BYTES);
}

#[test]
fn a_response_outside_the_wires_time_range_isnt_written() {
    let far = RenewResponse {
        not_after: DateTime::<Utc>::MAX_UTC,
        ..response()
    };

    assert_eq!(
        wire::RenewResponse::try_from(far),
        Err(ConvertError::new(
            "RenewResponse.not_after_unix_ms",
            Problem::OutOfRange
        ))
    );
}

#[test]
fn a_request_is_empty_on_the_wire() {
    assert_eq!(wire::RenewRequest {}.encode_to_vec(), Vec::<u8>::new());
    assert_eq!(
        wire::RenewRequest::decode([].as_slice()),
        Ok(wire::RenewRequest {})
    );
}

#[test]
fn the_agent_waits_longer_than_the_server_signs_and_both_messages_fit_the_streams_limits() {
    assert!(RENEW_SERVER_DEADLINE < RENEW_CLIENT_DEADLINE);
    // The control service has one pair of size limits, the stream's.
    let largest = wire::RenewResponse {
        certificate_pem: "C".repeat(MAX_CERTIFICATE_BYTES),
        ..wire_response()
    };
    assert!(largest.encoded_len() < MAX_SERVER_MESSAGE_BYTES);
    assert!(wire::RenewRequest {}.encoded_len() < MAX_AGENT_MESSAGE_BYTES);
}
