//! P10.9's loopback suite: the real managed agents against the real
//! server, over mTLS on `127.0.0.1` through their proxies, the bots on the
//! fake connector (Plan.md P10.9; ADR-0019). Group C tested the
//! rejections; these are the cases that must hold.

use fleet_core::audit::RecordedValue;
use fleet_server::ports::audit::AuditRecord;

mod agent_restart;
mod chat;
mod killed_mid_stream;
mod outage;
mod server_restart;
mod silent_connection;

/// The `reason` an audit entry records, such as a stream end's.
pub(crate) fn reason(record: &AuditRecord) -> Option<String> {
    record
        .metadata
        .entries()
        .iter()
        .find(|(key, _)| key == "reason")
        .and_then(|(_, value)| match value {
            RecordedValue::Text(text) => Some(text.clone()),
            _ => None,
        })
}
