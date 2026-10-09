//! [`ModePreset`]: the built-in modes, by name.

use core::fmt;
use core::str::FromStr;

use super::ModeDefinition;

/// Why a name isn't a [`ModePreset`]. The message lists the valid names and
/// never the text that was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("unknown mode preset; expected one of: {names}", names = PresetNames)]
pub struct UnknownPresetError;

/// A built-in mode, named as the standalone agent's config names it
/// (`mode = "afk"`). P11.1 gives each preset a fixed ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModePreset {
    /// [`ModeDefinition::afk`].
    Afk,
    /// [`ModeDefinition::farm`].
    Farm,
}

impl ModePreset {
    /// Every preset, in a fixed order.
    pub const ALL: [Self; 2] = [Self::Afk, Self::Farm];

    /// The preset's name: lowercase, as configs write it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Afk => "afk",
            Self::Farm => "farm",
        }
    }

    /// The preset's mode.
    #[must_use]
    pub fn definition(self) -> ModeDefinition {
        match self {
            Self::Afk => ModeDefinition::afk(),
            Self::Farm => ModeDefinition::farm(),
        }
    }
}

impl FromStr for ModePreset {
    type Err = UnknownPresetError;

    /// Reads a preset's exact name; the case must match.
    fn from_str(name: &str) -> Result<Self, UnknownPresetError> {
        Self::ALL
            .into_iter()
            .find(|preset| preset.name() == name)
            .ok_or(UnknownPresetError)
    }
}

impl fmt::Display for ModePreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The names of [`ModePreset::ALL`], separated by `, `.
struct PresetNames;

impl fmt::Display for PresetNames {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, preset) in ModePreset::ALL.into_iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            f.write_str(preset.name())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::afk("afk", ModePreset::Afk)]
    #[case::farm("farm", ModePreset::Farm)]
    fn parses_its_exact_names(#[case] name: &str, #[case] preset: ModePreset) {
        assert_eq!(name.parse(), Ok(preset));
    }

    #[rstest]
    #[case::upper_case("AFK")]
    #[case::title_case("Farm")]
    #[case::leading_space(" afk")]
    #[case::trailing_space("afk ")]
    #[case::empty("")]
    #[case::unknown("fishing")]
    fn rejects_anything_else(#[case] name: &str) {
        assert_eq!(name.parse::<ModePreset>(), Err(UnknownPresetError));
    }

    #[test]
    fn every_name_parses_back_to_its_preset() {
        for preset in ModePreset::ALL {
            assert_eq!(preset.name().parse(), Ok(preset));
            assert_eq!(preset.to_string(), preset.name());
        }
    }

    #[test]
    fn names_are_lowercase_and_unique() {
        let names = ModePreset::ALL.map(ModePreset::name);
        assert_eq!(names, ["afk", "farm"]);
    }

    #[test]
    fn definitions_are_the_built_in_modes() {
        assert_eq!(ModePreset::Afk.definition(), ModeDefinition::afk());
        assert_eq!(ModePreset::Farm.definition(), ModeDefinition::farm());
    }

    #[test]
    fn the_error_lists_the_valid_names_and_never_the_input() {
        let error = "fishing".parse::<ModePreset>().unwrap_err();
        assert_eq!(
            error.to_string(),
            "unknown mode preset; expected one of: afk, farm"
        );
        assert!(!error.to_string().contains("fishing"));
    }
}
