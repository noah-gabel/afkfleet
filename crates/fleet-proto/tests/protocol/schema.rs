//! The wire schema is pinned: every change to a message, a field's number or
//! type, an enum, the service or the redacted messages changes this snapshot,
//! so it shows up in review next to the protocol version (ADR-0019).

use fleet_proto::PROTOCOL_VERSION;

use crate::WIRE_SCHEMA;

#[test]
fn the_wire_schema_is_pinned() {
    insta::assert_snapshot!(
        "wire_schema",
        format!("protocol version {PROTOCOL_VERSION}\n\n{WIRE_SCHEMA}")
    );
}
