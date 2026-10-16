//! The enrollment's messages (Plan.md P10.3; ADR-0019): they cross the wire
//! unchanged, every broken field is refused with its error, the token never
//! prints, and the limits hold together.

use chrono::{DateTime, Utc};
use fleet_core::value::AgentNameError;
use fleet_proto::enrollment::{
    CLIENT_DEADLINE, EnrollRequest, EnrollResponse, MAX_CERTIFICATE_BYTES, MAX_CSR_BYTES,
    MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, SERVER_DEADLINE,
};
use fleet_proto::generated as wire;
use fleet_proto::{ConvertError, Problem};
use prost::Message as _;
use rstest::rstest;
use secrecy::{ExposeSecret as _, SecretString};
use uuid::Uuid;

const TOKEN: &str = "afk_et_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq";
const AGENT: &str = "018bcfe5-6800-7bab-abab-abababababab";
const V4: &str = "550e8400-e29b-41d4-a716-446655440000";
const AT: i64 = 1_790_000_000_000;

fn at() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(AT).unwrap()
}

fn wire_request() -> wire::EnrollRequest {
    wire::EnrollRequest {
        token: TOKEN.to_owned(),
        name: "agent-1".to_owned(),
        csr_der: vec![0x30; 150],
    }
}

/// A valid request with `change` made to it: `EnrollRequest` wipes its token
/// when dropped, so struct-update syntax can't build one.
fn request_with(change: impl FnOnce(&mut wire::EnrollRequest)) -> wire::EnrollRequest {
    let mut request = wire_request();
    change(&mut request);
    request
}

fn response() -> EnrollResponse {
    EnrollResponse {
        agent_id: AGENT.parse().unwrap(),
        certificate_pem: "C".repeat(400),
        not_after: at(),
    }
}

fn wire_response() -> wire::EnrollResponse {
    wire::EnrollResponse::try_from(response()).unwrap()
}

#[test]
fn a_request_crosses_the_wire_unchanged() {
    let request = EnrollRequest {
        token: SecretString::from(TOKEN),
        name: "agent-1".parse().unwrap(),
        csr_der: vec![1, 2, 3],
    };

    let bytes = wire::EnrollRequest::from(request).encode_to_vec();
    let again =
        EnrollRequest::try_from(wire::EnrollRequest::decode(bytes.as_slice()).unwrap()).unwrap();

    assert_eq!(again.token.expose_secret(), TOKEN);
    assert_eq!(again.name.as_str(), "agent-1");
    assert_eq!(again.csr_der, [1, 2, 3]);
}

#[test]
fn a_response_crosses_the_wire_unchanged() {
    let bytes = wire_response().encode_to_vec();

    let again =
        EnrollResponse::try_from(wire::EnrollResponse::decode(bytes.as_slice()).unwrap()).unwrap();

    assert_eq!(again, response());
}

#[test]
fn a_token_is_moved_as_it_came_whatever_its_shape() {
    for token in ["", "x", "afk_at_another-kind", "with space"] {
        let request =
            EnrollRequest::try_from(request_with(|request| request.token = token.to_owned()))
                .unwrap();

        assert_eq!(request.token.expose_secret(), token);
    }
}

#[rstest]
#[case::bad_name(
    request_with(|request| request.name = ".hidden".to_owned()),
    ConvertError::new("EnrollRequest.name", Problem::AgentName(AgentNameError::InvalidStart))
)]
#[case::no_csr(
    request_with(|request| request.csr_der = Vec::new()),
    ConvertError::new("EnrollRequest.csr_der", Problem::Missing)
)]
#[case::csr_too_large(
    request_with(|request| request.csr_der = vec![0x30; MAX_CSR_BYTES + 1]),
    ConvertError::new("EnrollRequest.csr_der", Problem::OutOfRange)
)]
fn a_broken_request_is_refused(#[case] request: wire::EnrollRequest, #[case] error: ConvertError) {
    assert_eq!(EnrollRequest::try_from(request).map(|_| ()), Err(error));
}

#[test]
fn the_largest_csr_is_accepted() {
    let request = EnrollRequest::try_from(request_with(|request| {
        request.csr_der = vec![0x30; MAX_CSR_BYTES];
    }))
    .unwrap();

    assert_eq!(request.csr_der.len(), MAX_CSR_BYTES);
}

#[rstest]
#[case::short_id(
    wire::EnrollResponse { agent_id: vec![1; 15], ..wire_response() },
    ConvertError::new("EnrollResponse.agent_id", Problem::WrongLength { len: 15 })
)]
#[case::v4_id(
    wire::EnrollResponse { agent_id: V4.parse::<Uuid>().unwrap().as_bytes().to_vec(), ..wire_response() },
    ConvertError::new("EnrollResponse.agent_id", Problem::NotV7)
)]
#[case::no_certificate(
    wire::EnrollResponse { certificate_pem: String::new(), ..wire_response() },
    ConvertError::new("EnrollResponse.certificate_pem", Problem::Missing)
)]
#[case::certificate_too_large(
    wire::EnrollResponse { certificate_pem: "C".repeat(MAX_CERTIFICATE_BYTES + 1), ..wire_response() },
    ConvertError::new("EnrollResponse.certificate_pem", Problem::OutOfRange)
)]
#[case::no_expiry(
    wire::EnrollResponse { not_after_unix_ms: None, ..wire_response() },
    ConvertError::new("EnrollResponse.not_after_unix_ms", Problem::Missing)
)]
#[case::expiry_out_of_range(
    wire::EnrollResponse { not_after_unix_ms: Some(i64::MAX), ..wire_response() },
    ConvertError::new("EnrollResponse.not_after_unix_ms", Problem::OutOfRange)
)]
fn a_broken_response_is_refused(
    #[case] response: wire::EnrollResponse,
    #[case] error: ConvertError,
) {
    assert_eq!(EnrollResponse::try_from(response), Err(error));
}

#[test]
fn a_response_outside_the_wire_s_time_range_isnt_written() {
    let far = EnrollResponse {
        not_after: DateTime::<Utc>::MAX_UTC,
        ..response()
    };

    assert_eq!(
        wire::EnrollResponse::try_from(far),
        Err(ConvertError::new(
            "EnrollResponse.not_after_unix_ms",
            Problem::OutOfRange
        ))
    );
}

#[test]
fn an_enroll_request_prints_its_name_only() {
    let request = wire_request();

    assert_eq!(
        format!("{request:?}"),
        "EnrollRequest { name: \"agent-1\", .. }"
    );
    for output in [
        format!("{request:?}"),
        format!("{request:#?}"),
        format!("{:?}", tonic::Request::new(wire_request())),
    ] {
        assert!(!output.contains(TOKEN), "the token printed: {output}");
        assert!(!output.contains("ABCDEFGH"), "the token printed: {output}");
    }
}

#[test]
fn the_validated_request_keeps_its_token_secret() {
    let request = EnrollRequest::try_from(wire_request()).unwrap();

    for output in [format!("{request:?}"), format!("{request:#?}")] {
        assert!(!output.contains("ABCDEFGH"), "the token printed: {output}");
        assert!(output.contains("agent-1"), "{output}");
    }
}

#[test]
fn a_refused_request_never_shows_its_token() {
    let error = EnrollRequest::try_from(request_with(|request| {
        request.name = format!("{TOKEN} as a name");
    }))
    .unwrap_err();

    for output in [error.to_string(), format!("{error:?}")] {
        assert!(!output.contains("ABCDEFGH"), "the token printed: {output}");
    }
}

#[test]
fn the_limits_hold_together() {
    assert!(SERVER_DEADLINE < CLIENT_DEADLINE);
    // The largest valid request fits its limit: a token, the longest name
    // and the largest CSR, with a little framing.
    let largest = wire::EnrollRequest {
        token: TOKEN.to_owned(),
        name: "a".repeat(64),
        csr_der: vec![0x30; MAX_CSR_BYTES],
    };
    assert!(largest.encoded_len() < MAX_REQUEST_BYTES);
    let largest = wire::EnrollResponse {
        certificate_pem: "C".repeat(MAX_CERTIFICATE_BYTES),
        ..wire_response()
    };
    assert!(largest.encoded_len() < MAX_RESPONSE_BYTES);
}
