//! Validated value objects: where a bot connects, the names of Minecraft
//! accounts, app users and agents, Minecraft profiles' UUIDs, and what an
//! agent reports about itself.
//!
//! Each type has a fallible constructor (`TryFrom<&str>`, or `TryFrom<u32>`
//! for a number), so code that holds one never has to check it again. serde
//! goes through the same constructor, so stored or received values are
//! validated too (ADR-0010).

mod agent_capacity;
mod agent_name;
mod agent_version;
mod mc_username;
mod mc_uuid;
mod server_address;
mod username;

pub use agent_capacity::{AgentCapacity, AgentCapacityError};
pub use agent_name::{AgentName, AgentNameError};
pub use agent_version::{AgentVersion, AgentVersionError};
pub use mc_username::{McUsername, McUsernameError};
pub use mc_uuid::{McUuid, McUuidError};
pub use server_address::{Host, ServerAddress, ServerAddressError};
pub use username::{Username, UsernameError};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::ChatMessage;
    use serde::Serialize;

    #[test]
    fn value_objects_serialize_as_plain_strings() {
        #[derive(Serialize)]
        struct Values {
            server: ServerAddress,
            ipv6_server: ServerAddress,
            mc_username: McUsername,
            username: Username,
            chat: ChatMessage,
        }
        let values = Values {
            server: "Mc.Example.COM:25566".parse().unwrap(),
            ipv6_server: "2001:db8::1".parse().unwrap(),
            mc_username: "AfkBot1".parse().unwrap(),
            username: "Alice".parse().unwrap(),
            chat: "  /spawn home ".parse().unwrap(),
        };

        insta::assert_json_snapshot!("value_objects_json", values);
    }
}
