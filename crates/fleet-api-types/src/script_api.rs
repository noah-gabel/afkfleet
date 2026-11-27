//! The script API's description for the app's editor (Plan.md P15.24, P15.25;
//! ADR-0036, D4; ADR-0045): every object a script uses with each member's
//! signature and summary, the hooks, the reasons a result can carry, and
//! the reference's examples as starter templates.
//!
//! It is `fleet_script`'s own description ([`fleet_script::API`],
//! [`fleet_script::reasons`], [`fleet_script::EXAMPLES`]) in the shape the
//! UI reads. `just gen` writes it as the constant `SCRIPT_API` in
//! `packages/ui/src/generated/script-api.ts` ([`crate::typescript`]), so the
//! editor's completion, its checks of a member's name and its hover help
//! all read the description the engine is tested against, never a list of
//! the UI's own. It never crosses the HTTP API or an IPC call.

use fleet_core::script::{Script, ScriptApiLevel, ScriptError};
use fleet_script::{EXAMPLES, MemberKind};
use serde::Serialize;
use ts_rs::TS;

use crate::script::ModeScript;

/// The script API, as the editor reads it.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct ScriptApi {
    /// Every object, in the reference's order: the game objects, then
    /// `input` (what `keys.forward` and `mouse.left` are) and `hooks` (the
    /// functions a script defines).
    pub objects: Vec<ScriptApiObject>,
    /// Every reason an action's result can carry in `result.reason`.
    pub reasons: Vec<String>,
    /// The reference's examples, each a complete scripted mode to start
    /// from.
    pub examples: Vec<ScriptExample>,
}

/// An object of the script API, or a group of names that belong together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct ScriptApiObject {
    /// Its name in a script: `keys`.
    pub name: String,
    /// What it's for, in a line.
    pub summary: String,
    /// Its members.
    pub members: Vec<ScriptApiMember>,
}

/// A member of an object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct ScriptApiMember {
    /// Its name.
    pub name: String,
    /// What it is.
    pub kind: ScriptApiMemberKind,
    /// How it's written, with its parameters: `hold_for(ms)`.
    pub signature: String,
    /// What it does or is, in a line.
    pub summary: String,
    /// The object of this API a property is, when it's one: `keys.forward`
    /// is an `input`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub object: Option<String>,
}

/// What a member of an object is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ScriptApiMemberKind {
    /// A function called with parentheses: `keys.jump.tap()`.
    Method,
    /// A value read without them: `bot.health`.
    Property,
    /// A function a script defines and the runtime calls.
    Hook,
}

impl From<MemberKind> for ScriptApiMemberKind {
    fn from(kind: MemberKind) -> Self {
        match kind {
            MemberKind::Method => Self::Method,
            // A map's field reads as a property does.
            MemberKind::Property | MemberKind::Field => Self::Property,
            MemberKind::Hook => Self::Hook,
        }
    }
}

/// The newest API level the editor offers: level 1 until the server
/// stamps the level a script needs, with the wire at level 2 (Plan.md
/// P16.15). Until then a mode is saved at level 1, so the editor offers
/// nothing a save would refuse (ADR-0052).
const OFFERED: ScriptApiLevel = ScriptApiLevel::V1;

/// How many of the reasons, in fleet-core's order, a level-1 script can
/// meet: the first ten (Phase 15's).
const LEVEL_1_REASONS: usize = 10;

/// One of the reference's examples: a complete scripted mode to start from.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct ScriptExample {
    /// A short name for it: `afk_farm`.
    pub id: String,
    /// Its heading in the reference.
    pub title: String,
    /// The mode's body, as an editor of the mode reads one: the script
    /// with its settings and declarations.
    pub script: ModeScript,
}

impl ScriptApi {
    /// fleet-script's description, its reasons and its examples, as far as
    /// level 1 goes until the server stamps a script's level (P16.15): an
    /// object, a member, a reason and an example of a
    /// newer level are left out.
    ///
    /// # Errors
    /// [`ScriptError`] if an example breaks one of fleet-core's rules for a
    /// scripted mode: fleet-script's own tests compile and run each, so
    /// it never does.
    pub fn describe() -> Result<Self, ScriptError> {
        let objects = fleet_script::API
            .iter()
            .filter(|object| object.level <= OFFERED)
            .map(|object| ScriptApiObject {
                name: object.name.to_owned(),
                summary: object.summary.to_owned(),
                members: object
                    .members
                    .iter()
                    .filter(|member| member.level <= OFFERED)
                    .map(|member| ScriptApiMember {
                        name: member.name.to_owned(),
                        kind: member.kind.into(),
                        signature: member.signature.to_owned(),
                        summary: member.summary.to_owned(),
                        object: member.object.map(str::to_owned),
                    })
                    .collect(),
            })
            .collect();
        let examples = EXAMPLES
            .iter()
            .filter(|example| example.draft().api <= OFFERED.get())
            .map(|example| {
                let script = Script::try_from(example.draft())?;
                Ok(ScriptExample {
                    id: example.id.to_owned(),
                    title: example.title.to_owned(),
                    script: ModeScript::of(&script, true),
                })
            })
            .collect::<Result<_, ScriptError>>()?;
        Ok(Self {
            objects,
            reasons: fleet_script::reasons()
                .into_iter()
                .take(LEVEL_1_REASONS)
                .map(str::to_owned)
                .collect(),
            examples,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_holds_every_level_1_object_and_member_of_fleet_scripts_description_in_its_order() {
        let api = ScriptApi::describe().unwrap();
        let offered: Vec<&fleet_script::Object> = fleet_script::API
            .iter()
            .filter(|object| object.level == ScriptApiLevel::V1)
            .collect();

        assert_eq!(api.objects.len(), offered.len());
        for (shown, object) in api.objects.iter().zip(offered) {
            assert_eq!(shown.name, object.name);
            assert_eq!(shown.summary, object.summary);
            let members: Vec<(&str, &str, &str, Option<&str>)> = shown
                .members
                .iter()
                .map(|member| {
                    (
                        member.name.as_str(),
                        member.signature.as_str(),
                        member.summary.as_str(),
                        member.object.as_deref(),
                    )
                })
                .collect();
            let described: Vec<(&str, &str, &str, Option<&str>)> = object
                .members
                .iter()
                .filter(|member| member.level == ScriptApiLevel::V1)
                .map(|member| (member.name, member.signature, member.summary, member.object))
                .collect();
            assert_eq!(members, described, "{}", object.name);
        }
    }

    #[test]
    fn a_members_kind_is_its_own() {
        assert_eq!(
            ScriptApiMemberKind::from(MemberKind::Method),
            ScriptApiMemberKind::Method
        );
        assert_eq!(
            ScriptApiMemberKind::from(MemberKind::Property),
            ScriptApiMemberKind::Property
        );
        assert_eq!(
            ScriptApiMemberKind::from(MemberKind::Hook),
            ScriptApiMemberKind::Hook
        );
        assert_eq!(
            ScriptApiMemberKind::from(MemberKind::Field),
            ScriptApiMemberKind::Property
        );
    }

    #[test]
    fn level_2_stays_out_of_the_editor_until_the_server_stamps_the_level() {
        let api = ScriptApi::describe().unwrap();
        let names: Vec<&str> = api
            .objects
            .iter()
            .map(|object| object.name.as_str())
            .collect();

        for newer in ["inventory", "menu", "sign", "clock", "slot", "time"] {
            assert!(!names.contains(&newer), "{newer}");
        }
        let hooks = api
            .objects
            .iter()
            .find(|object| object.name == "hooks")
            .unwrap();
        assert!(hooks.members.iter().all(|hook| hook.name != "on_menu_open"));
        let bot = api
            .objects
            .iter()
            .find(|object| object.name == "bot")
            .unwrap();
        assert!(bot.members.iter().all(|member| member.name != "name"));
        assert_eq!(api.reasons.len(), 10);
        assert_eq!(api.reasons.last().map(String::as_str), Some("no_command"));
    }

    #[test]
    fn it_holds_the_reasons_and_every_example_as_an_editor_reads_its_mode() {
        let api = ScriptApi::describe().unwrap();

        assert_eq!(api.reasons, fleet_script::reasons()[..LEVEL_1_REASONS]);
        let ids: Vec<&str> = api
            .examples
            .iter()
            .map(|example| example.id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "afk_farm",
                "minecart_farm",
                "cobblestone_generator",
                "balance"
            ]
        );
        for (shown, example) in api
            .examples
            .iter()
            .zip(EXAMPLES.iter().filter(|example| example.draft().api == 1))
        {
            assert_eq!(shown.title, example.title);
            assert_eq!(shown.script.source.as_str(), example.source);
            assert!(
                shown
                    .script
                    .messages
                    .iter()
                    .all(|message| !message.arguments_hidden)
            );
        }
        let cobblestone = &api.examples[2].script;
        assert_eq!(cobblestone.block_guard[0].as_str(), "minecraft:cobblestone");
        let balance = &api.examples[3].script;
        assert_eq!(balance.messages.len(), 2);
        assert_eq!(balance.patterns.len(), 1);
    }

    #[test]
    fn it_serializes_a_member_without_an_object_without_the_field() {
        let api = ScriptApi::describe().unwrap();
        let json = serde_json::to_value(&api).unwrap();

        assert_eq!(json["objects"][0]["name"], "keys");
        assert_eq!(json["objects"][0]["members"][0]["object"], "input");
        assert_eq!(json["objects"][0]["members"][0]["kind"], "property");
        let release_all = &json["objects"][0]["members"][10];
        assert_eq!(release_all["name"], "release_all");
        assert!(release_all.get("object").is_none());
    }
}
