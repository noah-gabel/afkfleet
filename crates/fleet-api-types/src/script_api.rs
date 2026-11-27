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
use fleet_script::{EXAMPLES, MemberKind, ObjectKind};
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
    /// What it is to a script: a name it uses, a value, or the hooks.
    pub kind: ScriptApiObjectKind,
    /// What it's for, in a line.
    pub summary: String,
    /// Its members.
    pub members: Vec<ScriptApiMember>,
    /// The script API level it came with: 1 or 2.
    pub level: u8,
}

/// What an object of the script API is to a script (Plan.md P16.16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ScriptApiObjectKind {
    /// A name a script uses to reach the game: `keys`, `menu`.
    Game,
    /// What a member is or a method answers, never named by a script:
    /// `input`, a `slot` map, a `time` map.
    Value,
    /// The functions a script defines.
    Hooks,
}

impl From<ObjectKind> for ScriptApiObjectKind {
    fn from(kind: ObjectKind) -> Self {
        match kind {
            ObjectKind::Game => Self::Game,
            ObjectKind::Value => Self::Value,
            ObjectKind::Hooks => Self::Hooks,
        }
    }
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
    /// The map of this API that a method answers, when it answers one:
    /// `menu.slot(n)` answers a `slot`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub answers: Option<String>,
    /// The script API level it came with: 1 or 2.
    pub level: u8,
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

/// The newest API level the editor offers: the newest there is, since the
/// server stamps the level a script needs (Plan.md P16.15; ADR-0052).
const OFFERED: ScriptApiLevel = ScriptApiLevel::NEWEST;

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
    /// fleet-script's description, its reasons and its examples, each with
    /// the API level it came with: the editor says which a script uses, and
    /// the server stamps the level it needs (P16.15).
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
                kind: object.kind().into(),
                summary: object.summary.to_owned(),
                level: object.level.get(),
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
                        answers: member.answers.map(str::to_owned),
                        level: member.level.get(),
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
                .map(str::to_owned)
                .collect(),
            examples,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A member as the test compares it: name, signature, summary, what it
    /// is, what it answers, and its level.
    type Compared<'a> = (
        &'a str,
        &'a str,
        &'a str,
        Option<&'a str>,
        Option<&'a str>,
        u8,
    );

    #[test]
    fn it_holds_every_object_and_member_of_fleet_scripts_description_in_its_order() {
        let api = ScriptApi::describe().unwrap();

        assert_eq!(api.objects.len(), fleet_script::API.len());
        for (shown, object) in api.objects.iter().zip(fleet_script::API) {
            assert_eq!(shown.name, object.name);
            assert_eq!(shown.summary, object.summary);
            assert_eq!(shown.level, object.level.get());
            let members: Vec<Compared<'_>> = shown
                .members
                .iter()
                .map(|member| {
                    (
                        member.name.as_str(),
                        member.signature.as_str(),
                        member.summary.as_str(),
                        member.object.as_deref(),
                        member.answers.as_deref(),
                        member.level,
                    )
                })
                .collect();
            let described: Vec<Compared<'_>> = object
                .members
                .iter()
                .map(|member| {
                    (
                        member.name,
                        member.signature,
                        member.summary,
                        member.object,
                        member.answers,
                        member.level.get(),
                    )
                })
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

    /// The server stamps the level a script needs (Plan.md P16.15), so the
    /// editor offers level 2, saying which name came with it, and knows the
    /// maps a script reads as values, never as names it uses.
    #[test]
    fn level_2_is_offered_with_each_names_level_and_its_maps_as_values() {
        let api = ScriptApi::describe().unwrap();
        let object = |name: &str| {
            api.objects
                .iter()
                .find(|object| object.name == name)
                .unwrap()
        };
        let kinds: Vec<(&str, ScriptApiObjectKind, u8)> = api
            .objects
            .iter()
            .map(|object| (object.name.as_str(), object.kind, object.level))
            .filter(|(name, _, _)| {
                ["keys", "menu", "clock", "input", "slot", "time", "hooks"].contains(name)
            })
            .collect();

        assert_eq!(
            kinds,
            [
                ("keys", ScriptApiObjectKind::Game, 1),
                ("input", ScriptApiObjectKind::Value, 1),
                ("menu", ScriptApiObjectKind::Game, 2),
                ("clock", ScriptApiObjectKind::Game, 2),
                ("slot", ScriptApiObjectKind::Value, 2),
                ("time", ScriptApiObjectKind::Value, 2),
                ("hooks", ScriptApiObjectKind::Hooks, 1),
            ]
        );
        let name = object("bot")
            .members
            .iter()
            .find(|member| member.name == "name")
            .unwrap();
        assert_eq!(name.level, 2);
        let slot = object("menu")
            .members
            .iter()
            .find(|member| member.name == "slot")
            .unwrap();
        assert_eq!(slot.answers.as_deref(), Some("slot"));
        assert!(
            object("hooks")
                .members
                .iter()
                .any(|hook| hook.name == "on_menu_open" && hook.level == 2)
        );
    }

    #[test]
    fn it_holds_the_reasons_and_every_example_as_an_editor_reads_its_mode() {
        let api = ScriptApi::describe().unwrap();

        assert_eq!(api.reasons, fleet_script::reasons());
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
                "balance",
                "sell_macro",
                "auction_scan",
                "farm_that_sells",
                "auction_lister",
                "auction_lister_anvil"
            ]
        );
        for (shown, example) in api.examples.iter().zip(EXAMPLES.iter()) {
            assert_eq!(shown.title, example.title);
            assert_eq!(shown.script.source.as_str(), example.source);
            // At its own level, which the server stamps it with too.
            assert_eq!(shown.script.api, example.level.get(), "{}", example.id);
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
