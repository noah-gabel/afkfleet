//! The standalone bots' specs, each with a fresh random v7 ID.
//!
//! A standalone agent persists nothing, so every start mints new IDs; the
//! run logs each one with its bot's username, server and mode, so logs can
//! be matched to the config after a restart (ADR-0014).

use chrono::{DateTime, Utc};
use fleet_core::bot::{BotSpec, DesiredRunState};
use fleet_core::id::{BotId, IdError};

use crate::config::StandaloneBot;

/// Why the run can't start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum StartupError {
    /// The config is for managed mode, which arrives in Phase 10.
    #[error(
        "managed mode ([control_plane]) arrives in Phase 10; this agent runs only [standalone] bots"
    )]
    ManagedMode,
    /// The OS's random source failed.
    #[error("the OS's random source failed: {0}")]
    Random(getrandom::Error),
    /// The start time can't go into a bot ID.
    #[error("the start time can't go into a bot ID: {0}")]
    BotId(IdError),
    /// The fleet can't be built from the config.
    #[error("the fleet can't be set up: {0}")]
    Setup(fleet_runtime::FleetSetupError),
}

/// The 10 random bytes of a v7 ID, from the OS's random source.
pub(crate) fn os_random() -> Result<[u8; 10], getrandom::Error> {
    let mut bytes = [0; 10];
    getrandom::fill(&mut bytes)?;
    Ok(bytes)
}

/// One running spec per bot, in the config's order, each with a new v7 ID
/// made from `anchor` and the bytes `random` gives.
///
/// # Errors
/// [`StartupError::Random`] if `random` fails, [`StartupError::BotId`] if
/// `anchor` can't go into a v7 ID (before 1970).
pub(crate) fn standalone_specs(
    bots: &[StandaloneBot],
    anchor: DateTime<Utc>,
    mut random: impl FnMut() -> Result<[u8; 10], getrandom::Error>,
) -> Result<Vec<BotSpec>, StartupError> {
    bots.iter()
        .map(|bot| {
            let bytes = random().map_err(StartupError::Random)?;
            let id = BotId::new_v7(anchor, bytes).map_err(StartupError::BotId)?;
            Ok(BotSpec {
                id,
                account: bot.account(),
                server: bot.server.clone(),
                mode: bot.mode.definition(),
                desired: DesiredRunState::Running,
                conflict_texts: bot.conflict_texts.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use fleet_core::bot::{BotAccount, DesiredRunState};
    use fleet_core::disconnect::ConflictTexts;
    use fleet_core::mode::ModePreset;

    use super::*;

    fn bot(name: &str, mode: ModePreset) -> StandaloneBot {
        StandaloneBot {
            username: name.parse().unwrap(),
            server: "localhost:25566".try_into().unwrap(),
            mode,
            conflict_texts: ConflictTexts::try_new(&["logged in from another location"]).unwrap(),
        }
    }

    fn anchor() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    /// Hands out 0x01…, 0x02…, … so each ID differs.
    fn counting() -> impl FnMut() -> Result<[u8; 10], getrandom::Error> {
        let mut next = 0_u8;
        move || {
            next += 1;
            Ok([next; 10])
        }
    }

    #[test]
    fn each_bot_gets_a_running_spec_from_its_entry() {
        let bots = [
            bot("AfkBot1", ModePreset::Afk),
            bot("AfkBot2", ModePreset::Farm),
        ];

        let specs = standalone_specs(&bots, anchor(), counting()).unwrap();

        assert_eq!(specs.len(), 2);
        for (spec, bot) in specs.iter().zip(&bots) {
            assert_eq!(spec.account, BotAccount::Offline(bot.username.clone()));
            assert_eq!(spec.server, bot.server);
            assert_eq!(spec.mode, bot.mode.definition());
            assert_eq!(spec.desired, DesiredRunState::Running);
            assert_eq!(spec.conflict_texts, bot.conflict_texts);
        }
    }

    #[test]
    fn each_id_is_a_v7_id_with_the_start_time_and_its_own_random_bytes() {
        let bots = [
            bot("AfkBot1", ModePreset::Afk),
            bot("AfkBot2", ModePreset::Afk),
        ];

        let specs = standalone_specs(&bots, anchor(), counting()).unwrap();

        let ids: Vec<_> = specs.iter().map(|spec| spec.id).collect();
        assert_ne!(ids[0], ids[1]);
        for (position, id) in ids.iter().enumerate() {
            let expected =
                fleet_core::id::BotId::new_v7(anchor(), [u8::try_from(position + 1).unwrap(); 10])
                    .unwrap();
            assert_eq!(*id, expected);
        }
    }

    #[test]
    fn a_failing_random_source_fails_the_start() {
        let bots = [bot("AfkBot1", ModePreset::Afk)];

        let result = standalone_specs(&bots, anchor(), || Err(getrandom::Error::UNEXPECTED));

        assert_eq!(
            result,
            Err(StartupError::Random(getrandom::Error::UNEXPECTED))
        );
    }

    #[test]
    fn a_start_time_before_1970_fails_the_start() {
        let bots = [bot("AfkBot1", ModePreset::Afk)];
        let before_1970 = DateTime::from_timestamp(-1, 0).unwrap();

        let result = standalone_specs(&bots, before_1970, counting());

        assert!(matches!(result, Err(StartupError::BotId(_))), "{result:?}");
    }

    #[test]
    fn the_os_random_source_gives_bytes() {
        // Two draws of 80 bits are equal with negligible probability.
        assert_ne!(os_random().unwrap(), os_random().unwrap());
    }
}
