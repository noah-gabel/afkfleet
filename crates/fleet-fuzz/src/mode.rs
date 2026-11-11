//! The target over mode definitions: as the database and the agents store
//! them, and as the API takes them.

use fleet_api_types::modes::{CreateModeRequest, ModeSteps, UpdateModeRequest};
use fleet_core::mode::{ModeDefinition, ModeName};
use garde::Validate as _;

/// What the API's handlers rely on once garde accepted a mode request: its
/// name is a mode's name, and its steps are a mode's definition.
fn holds_what_the_handlers_expect(name: &str, steps: &ModeSteps) {
    assert!(
        ModeName::try_from(name).is_ok(),
        "garde accepted a name that isn't a mode's name"
    );
    assert!(
        steps.definition().is_ok(),
        "garde accepted steps that aren't a mode's definition"
    );
}

/// A mode definition's JSON over any bytes: no panic; a stored definition
/// that's accepted reads back as itself; and a request garde accepts holds
/// what the handlers then take for granted.
///
/// # Panics
/// When a property breaks.
pub fn mode_definition(data: &[u8]) {
    if let Ok(definition) = serde_json::from_slice::<ModeDefinition>(data) {
        let written = serde_json::to_vec(&definition);
        assert!(written.is_ok(), "an accepted definition can't be written");
        let again = written
            .ok()
            .and_then(|written| serde_json::from_slice::<ModeDefinition>(&written).ok());
        assert_eq!(
            again.as_ref(),
            Some(&definition),
            "a definition doesn't read back from what it writes"
        );
    }
    if let Ok(request) = serde_json::from_slice::<CreateModeRequest>(data)
        && request.validate().is_ok()
    {
        holds_what_the_handlers_expect(&request.name, &request.steps);
    }
    if let Ok(request) = serde_json::from_slice::<UpdateModeRequest>(data)
        && request.validate().is_ok()
    {
        holds_what_the_handlers_expect(&request.name, &request.steps);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "garde accepted a name that isn't a mode's name")]
    fn a_name_no_mode_may_have_fails() {
        holds_what_the_handlers_expect("", &ModeSteps(Vec::new()));
    }
}
