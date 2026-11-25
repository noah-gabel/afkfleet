//! "Convert to script": a step mode written as the script that does what
//! its steps do (Plan.md P15.27; ADR-0036, D17; ADR-0046).
//!
//! **Each step is a line of a hook,** marked as the mode's editor names it,
//! by its section and its place there: `// while it's online, step 2`.
//! - at start, in `on_join`, which runs at every join as the steps at start
//!   do; on respawn, in `on_respawn`;
//! - a repeating step gets a timer of its own, set in `on_join` and named
//!   after it (`online_2`): `timer.every` without jitter, and with jitter a
//!   `timer.after` of the interval and a random part of the jitter, set
//!   again at each run, so each gap is drawn as the plan draws it. Repeating
//!   steps next to each other on one schedule without jitter share a timer:
//!   they're always due together, in step order, as in the plan;
//! - a probability below 100 % is a `random.chance` around the step's line;
//!   a run it skips is due again all the same;
//! - an on-respawn chat goes at most once every 30 seconds, as the step did:
//!   a flag in `state` and a timer that clears it.
//!
//! **Each action is the script's call that does it:** a look `look.at`, a
//! turn `look.turn` with `random.between` scaled to the maximum's decimals,
//! a jump `keys.jump.tap()`, sneaking `keys.sneak.hold()` and `release()`,
//! a use `mouse.right.tap()`, holding it `mouse.right.hold()` and
//! `release()`, a hotbar slot `hotbar.select` (1 to 9), and chat
//! `chat.send` of a message the mode declares, so a chat step's text is
//! never in the script: below the right to edit the mode it's shown as a
//! step's is, a command's `/` and name alone (ADR-0044, E6). A swing and an attack are a left click, which on
//! an entity attacks and on anything else swings, so each checks
//! `bot.looking_at` first and clicks only where the step would have done
//! what it says.
//!
//! **What a script does differently** is returned as [`Note`]s at their
//! steps: a swing that skips an entity, an interval below a script's
//! shortest raised to it, steps that share a limit below the script, a gap
//! cut at a timer's 24 hours, a held right button that clicks again. The runtime's own differences, the same
//! for every mode (a script lets go of everything at a death, a respawn and
//! a world change, and its timers wait while the bot is dead), are the
//! reference's.

use core::fmt::Write as _;
use core::time::Duration;

use fleet_core::mode::{Action, ModeBody, ModeDefinition, Schedule, Step};
use fleet_core::script::{
    Key, LeashRadius, Limited, MessageDraft, MessageTemplate, Script, ScriptApiLevel, ScriptDraft,
    ScriptError, TemplateError, ToolGuard, limits,
};

/// A step mode as a script: the scripted mode's body, and what the script
/// does differently from the steps, step by step.
#[derive(Debug, Clone, PartialEq)]
pub struct Conversion {
    script: Script,
    notes: Vec<Note>,
}

impl Conversion {
    /// The scripted mode's body.
    #[must_use]
    pub const fn script(&self) -> &Script {
        &self.script
    }

    /// What the script does differently from the steps: by step, and a
    /// step's own differences before a limit it shares.
    #[must_use]
    pub fn notes(&self) -> &[Note] {
        &self.notes
    }
}

/// Something a converted step does differently in a script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Note {
    /// The step's place in the mode, from 0.
    pub step: usize,
    /// What's different.
    pub kind: NoteKind,
}

/// What a converted step does differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    /// A swing is a left click in a script, which hits an entity under the
    /// crosshair, so the step swings only while the crosshair is on none.
    SwingSkipsEntities,
    /// The step repeats at most every `every`, its kind's shortest in a
    /// script, not at its own shorter interval.
    Slower {
        /// The script's shortest interval for it.
        every: Duration,
    },
    /// A held right button uses again every 4 game ticks with an item that
    /// isn't used over time, which the step's hold didn't.
    HoldRepeats,
    /// A gap between two runs is at most 24 hours, a timer's longest.
    GapCapped,
    /// The step does what the step `other` does too, and a script does it
    /// at most once every `every`: a run that comes sooner after one of
    /// `other`'s is refused.
    SharesLimit {
        /// The other step's place, from 0.
        other: usize,
        /// The shortest time between two of them.
        every: Duration,
    },
}

/// Why a mode can't be converted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConvertError {
    /// The mode is a script already.
    #[error("the mode is a script already")]
    NotSteps,
    /// A chat step's text isn't one a mode may declare: a brace in a
    /// command's name, or an invisible character, which a step's chat may
    /// hold and a declaration may not (ADR-0036, D8).
    #[error("step {step}'s message can't be declared: {reason}")]
    Message {
        /// The step's place, from 0.
        step: usize,
        /// Why.
        reason: TemplateError,
    },
    /// The steps need more timers than a script has.
    #[error(
        "the mode needs {count} timers; a script has at most {max}",
        max = limits::MAX_TIMERS
    )]
    TooManyTimers {
        /// How many it needs.
        count: usize,
    },
    /// What was made isn't a scripted mode's body. No step mode leads here:
    /// the converter's property tests hold it.
    #[error("the converted script isn't a scripted mode's body: {0}")]
    Script(ScriptError),
}

/// The script's first lines.
const HEADER: &str = "// Converted from a step mode: each line marked with a step does what
// that step did. What a script does differently is in the script
// reference, under \"Converting a step mode\".
";

/// The name of the timer that ends an on-respawn chat's 30 seconds, and of
/// the flag in `state` that holds them.
const RESPAWN_CHAT: &str = "respawn_chat";

/// The shortest time between two on-respawn chats, as a step mode keeps it.
const RESPAWN_CHAT_GAP: Duration = ModeDefinition::MIN_CHAT_INTERVAL;

/// A left click at most this often: a swing's shortest interval in a script.
const LEFT_CLICK: Duration = ModeDefinition::MIN_ATTACK_INTERVAL;

const ATTACK: &str = "
// Attacks what the crosshair is on if it's an entity within reach, as the
// step did: a left click on anything else would swing.
fn attack() {
    let target = bot.looking_at;
    if target != () && target.kind == \"entity\" {
        mouse.left.tap();
    }
}
";

const SWING: &str = "
// Swings the arm as a left click does, unless the crosshair is on an
// entity, which a left click would hit.
fn swing() {
    let target = bot.looking_at;
    if target == () || target.kind != \"entity\" {
        mouse.left.tap();
    }
}
";

/// `definition`'s steps as a script, with what it does differently
/// ([`Conversion`]): each step a line of a hook, marked as the mode's
/// editor names it, as ADR-0046 (C1, C2) sets out.
///
/// # Errors
/// [`ConvertError::NotSteps`] for a scripted mode;
/// [`ConvertError::Message`] for the first chat step, in step order, whose
/// text a mode can't declare; [`ConvertError::TooManyTimers`] for a mode
/// that repeats more steps on schedules of their own than a script has
/// timers.
pub fn convert(definition: &ModeDefinition) -> Result<Conversion, ConvertError> {
    let ModeBody::Steps(steps) = definition.body() else {
        return Err(ConvertError::NotSteps);
    };
    let messages = steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| match &step.action {
            Action::SendChat { message } => Some(declared(steps, index, message.as_str())),
            _ => None,
        })
        .collect::<Result<Vec<_>, _>>()?;
    let schedules: Vec<Option<(Duration, Duration)>> = steps.iter().map(repeats).collect();
    let timers = timers(&schedules);
    let respawn_chat = steps.iter().any(is_respawn_chat);
    let count = timers.len() + usize::from(respawn_chat);
    if count > limits::MAX_TIMERS {
        return Err(ConvertError::TooManyTimers { count });
    }
    let source = write(steps, &timers, respawn_chat);
    let script = Script::try_from(ScriptDraft {
        api: ScriptApiLevel::NEWEST.get(),
        source,
        leash: LeashRadius::DEFAULT.blocks(),
        tool_guard: ToolGuard::DEFAULT.uses_left(),
        block_guard: Vec::new(),
        messages,
        patterns: Vec::new(),
    })
    .map_err(ConvertError::Script)?;
    Ok(Conversion {
        script,
        notes: notes(steps, &schedules),
    })
}

/// The chat step at `index` as a declared message: its key, the step's name
/// (`join_2`), and its text with each brace doubled, so it's sent as
/// written.
fn declared(steps: &[Step], index: usize, text: &str) -> Result<MessageDraft, ConvertError> {
    let draft = MessageDraft {
        key: name(steps, index),
        text: text.replace('{', "{{").replace('}', "}}"),
        values: Vec::new(),
    };
    MessageTemplate::try_from(draft.clone())
        .map(|_| draft)
        .map_err(|reason| ConvertError::Message {
            step: index,
            reason,
        })
}

/// Where the mode's editor shows the step at `index`: its section, by the
/// step's schedule, and its place among that section's steps, from 1.
fn place(steps: &[Step], index: usize) -> (Section, usize) {
    let section = steps
        .get(index)
        .map_or(Section::Join, |step| Section::of(step.schedule));
    let before = steps
        .iter()
        .take(index)
        .filter(|step| Section::of(step.schedule) == section)
        .count();
    (section, before.saturating_add(1))
}

/// A step's name in the script, for its message's key and its timer:
/// `join_2`, `online_1`, `respawn_1`.
fn name(steps: &[Step], index: usize) -> String {
    let (section, n) = place(steps, index);
    format!("{}_{n}", section.short())
}

/// A step as a comment names it: `while it's online, step 2`.
fn named(steps: &[Step], index: usize) -> String {
    let (section, n) = place(steps, index);
    format!("{}, step {n}", section.words())
}

/// A section of the mode's editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// "When it joins": the steps at start.
    Join,
    /// "While it's online": the repeating steps.
    Online,
    /// "After it respawns": the steps on respawn.
    Respawn,
}

impl Section {
    const fn of(schedule: Schedule) -> Self {
        match schedule {
            Schedule::AtStart => Self::Join,
            Schedule::Every { .. } => Self::Online,
            Schedule::OnRespawn => Self::Respawn,
        }
    }

    /// For a key or a timer's name.
    const fn short(self) -> &'static str {
        match self {
            Self::Join => "join",
            Self::Online => "online",
            Self::Respawn => "respawn",
        }
    }

    /// For a comment, as the editor's heading reads.
    const fn words(self) -> &'static str {
        match self {
            Self::Join => "when it joins",
            Self::Online => "while it's online",
            Self::Respawn => "after it respawns",
        }
    }
}

/// A repeating step's interval and jitter as a script runs them: the
/// interval raised to a script's shortest for it, the jitter cut so a gap
/// is at most a timer's longest. `None` for a step that doesn't repeat.
fn repeats(step: &Step) -> Option<(Duration, Duration)> {
    let Schedule::Every { interval, jitter } = step.schedule else {
        return None;
    };
    let interval = interval.max(shortest(&step.action));
    let jitter = jitter.min(limits::MAX_TIMER.saturating_sub(interval));
    Some((interval, jitter))
}

/// A script's shortest interval for `action`: a timer's 250 ms, and a left
/// click's 500 ms for a swing.
fn shortest(action: &Action) -> Duration {
    match action {
        Action::SwingArm => LEFT_CLICK,
        _ => limits::MIN_TIMER,
    }
}

/// A timer and the repeating steps it runs, in step order.
struct Timer {
    steps: Vec<usize>,
    interval: Duration,
    jitter: Duration,
}

/// The timers for `schedules`, one a repeating step, but one for steps
/// next to each other among the repeating ones on the same schedule
/// without jitter.
fn timers(schedules: &[Option<(Duration, Duration)>]) -> Vec<Timer> {
    let mut timers: Vec<Timer> = Vec::new();
    for (index, schedule) in schedules.iter().enumerate() {
        let Some((interval, jitter)) = *schedule else {
            continue;
        };
        match timers.last_mut() {
            Some(last)
                if jitter.is_zero() && last.jitter.is_zero() && last.interval == interval =>
            {
                last.steps.push(index);
            }
            _ => timers.push(Timer {
                steps: vec![index],
                interval,
                jitter,
            }),
        }
    }
    timers
}

fn is_respawn_chat(step: &Step) -> bool {
    step.schedule == Schedule::OnRespawn && matches!(step.action, Action::SendChat { .. })
}

/// The script's text.
fn write(steps: &[Step], timers: &[Timer], respawn_chat: bool) -> String {
    // At every join: the 30 seconds of an on-respawn chat start over, the
    // steps at start run, and the timers are set, each in step order.
    let mut join = Vec::new();
    if respawn_chat {
        join.push(format!("state.{RESPAWN_CHAT} = false;"));
    }
    for (index, step) in steps.iter().enumerate() {
        if step.schedule == Schedule::AtStart {
            join.extend(lines(steps, index));
        }
    }
    for timer in timers {
        join.push(format!(
            "{} // {}",
            arm(steps, timer),
            numbered(steps, &timer.steps)
        ));
    }
    let respawn: Vec<String> = steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.schedule == Schedule::OnRespawn)
        .flat_map(|(index, step)| {
            if is_respawn_chat(step) {
                respawn_chat_lines(steps, index)
            } else {
                lines(steps, index)
            }
        })
        .collect();
    let mut arms: Vec<String> = Vec::new();
    for timer in timers {
        let mut body: Vec<String> = timer
            .steps
            .iter()
            .flat_map(|index| lines(steps, *index))
            .collect();
        if !timer.jitter.is_zero() {
            body.push(arm(steps, timer));
        }
        arms.push(arm_of(&timer_name(steps, timer), &body));
    }
    if respawn_chat {
        arms.push(arm_of(
            RESPAWN_CHAT,
            &[format!("state.{RESPAWN_CHAT} = false;")],
        ));
    }

    let mut source = HEADER.to_owned();
    if !join.is_empty() {
        source.push_str(&function("fn on_join(first)", &join));
    }
    if !respawn.is_empty() {
        source.push_str(&function("fn on_respawn()", &respawn));
    }
    if !arms.is_empty() {
        let mut switch = vec!["switch name {".to_owned()];
        switch.extend(
            arms.iter()
                .flat_map(|arm| arm.lines().map(|line| format!("    {line}"))),
        );
        switch.push("}".to_owned());
        source.push_str(&function("fn on_timer(name)", &switch));
    }
    if steps
        .iter()
        .any(|step| step.action == Action::AttackFacingEntity)
    {
        source.push_str(ATTACK);
    }
    if steps.iter().any(|step| step.action == Action::SwingArm) {
        source.push_str(SWING);
    }
    source
}

/// A function with `lines` as its body, after a blank line.
fn function(signature: &str, lines: &[String]) -> String {
    let mut text = format!("\n{signature} {{\n");
    for line in lines {
        // Writing into a String never fails.
        let _ = writeln!(text, "    {line}");
    }
    text.push_str("}\n");
    text
}

/// A switch arm for `name` with `body`.
fn arm_of(name: &str, body: &[String]) -> String {
    let mut text = format!("\"{name}\" => {{\n");
    for line in body {
        let _ = writeln!(text, "    {line}");
    }
    text.push_str("},");
    text
}

/// A timer's name: its first step's.
fn timer_name(steps: &[Step], timer: &Timer) -> String {
    timer
        .steps
        .first()
        .map_or_else(String::new, |index| name(steps, *index))
}

/// The line that sets `timer`: `timer.every` without jitter, `timer.after`
/// with a random part of it.
fn arm(steps: &[Step], timer: &Timer) -> String {
    let name = timer_name(steps, timer);
    let interval = timer.interval.as_millis();
    if timer.jitter.is_zero() {
        format!("timer.every({interval}, \"{name}\");")
    } else {
        format!(
            "timer.after({interval} + random.between(0, {}), \"{name}\");",
            timer.jitter.as_millis()
        )
    }
}

/// A timer's steps as a comment names them: `while it's online, step 3`,
/// `while it's online, steps 3 and 4`, `while it's online, steps 3, 4 and
/// 5`. A timer's steps are all repeating ones.
fn numbered(steps: &[Step], timer: &[usize]) -> String {
    let numbers: Vec<String> = timer
        .iter()
        .map(|index| place(steps, *index).1.to_string())
        .collect();
    let words = Section::Online.words();
    match numbers.split_last() {
        Some((last, [])) => format!("{words}, step {last}"),
        Some((last, rest)) => format!("{words}, steps {} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// The step's lines: its call, marked with its name, inside a
/// `random.chance` when it doesn't always run.
fn lines(steps: &[Step], index: usize) -> Vec<String> {
    let Some(step) = steps.get(index) else {
        return Vec::new();
    };
    let line = format!(
        "{} // {}",
        call(steps, index, &step.action),
        named(steps, index)
    );
    if step.probability >= 100 {
        vec![line]
    } else {
        vec![
            format!("if random.chance({}) {{", step.probability),
            format!("    {line}"),
            "}".to_owned(),
        ]
    }
}

/// An on-respawn chat's lines: sent at most once every 30 seconds, as a
/// step mode sends it, the 30 seconds counted from a roll that came up.
fn respawn_chat_lines(steps: &[Step], index: usize) -> Vec<String> {
    let Some(step) = steps.get(index) else {
        return Vec::new();
    };
    let chance = if step.probability >= 100 {
        String::new()
    } else {
        format!(" && random.chance({})", step.probability)
    };
    vec![
        format!(
            "if !state.{RESPAWN_CHAT}{chance} {{ // {}, at most once every 30 seconds",
            named(steps, index)
        ),
        format!("    state.{RESPAWN_CHAT} = true;"),
        format!(
            "    timer.after({}, \"{RESPAWN_CHAT}\");",
            RESPAWN_CHAT_GAP.as_millis()
        ),
        format!("    {}", call(steps, index, &step.action)),
        "}".to_owned(),
    ]
}

/// The script's call that does `action`, the step at `index`.
fn call(steps: &[Step], index: usize, action: &Action) -> String {
    match action {
        Action::Look { yaw, pitch } => {
            format!("look.at({}, {});", decimal(*yaw), decimal(*pitch))
        }
        Action::RotateRandom { max_yaw, max_pitch } => {
            format!("look.turn({}, {});", turn(*max_yaw), turn(*max_pitch))
        }
        Action::Jump => "keys.jump.tap();".to_owned(),
        Action::Sneak { on: true } => "keys.sneak.hold();".to_owned(),
        Action::Sneak { on: false } => "keys.sneak.release();".to_owned(),
        Action::SwingArm => "swing();".to_owned(),
        Action::UseItem => "mouse.right.tap();".to_owned(),
        Action::HoldUse { on: true } => "mouse.right.hold();".to_owned(),
        Action::HoldUse { on: false } => "mouse.right.release();".to_owned(),
        Action::AttackFacingEntity => "attack();".to_owned(),
        Action::SelectHotbarSlot { slot } => {
            format!("hotbar.select({});", slot.get().saturating_add(1))
        }
        Action::SendChat { .. } => format!("chat.send(\"{}\");", name(steps, index)),
    }
}

/// An angle as a script writes it: the shortest decimal that is the same
/// `f32`, with a decimal point, so it reads back as itself.
fn decimal(angle: f32) -> String {
    let text = angle.to_string();
    if text.contains('.') {
        text
    } else {
        format!("{text}.0")
    }
}

/// A random amount from `-max` to `max`: whole degrees for a whole maximum,
/// and for one with decimals, a whole number of its smallest decimal
/// divided back, so the maximum itself can come up.
fn turn(max: f32) -> String {
    if max <= 0.0 {
        return "0".to_owned();
    }
    let text = max.to_string();
    let (whole, fraction) = text.split_once('.').unwrap_or((text.as_str(), ""));
    let digits = format!("{whole}{fraction}");
    let digits = match digits.trim_start_matches('0') {
        "" => "0",
        digits => digits,
    };
    let between = format!("random.between(-{digits}, {digits})");
    if fraction.is_empty() {
        between
    } else {
        format!("{between} / 1{}.0", "0".repeat(fraction.len()))
    }
}

/// The limit below a script that `action` falls under; none for a release
/// and a command, which have none.
fn limited(action: &Action) -> Option<Limited> {
    match action {
        Action::Look { .. } => Some(Limited::LookAt),
        Action::RotateRandom { .. } => Some(Limited::LookTurn),
        Action::Jump => Some(Limited::Key(Key::Jump)),
        Action::Sneak { on: true } => Some(Limited::Key(Key::Sneak)),
        Action::SwingArm | Action::AttackFacingEntity => Some(Limited::LeftButton),
        Action::UseItem => Some(Limited::RightTap),
        Action::HoldUse { on: true } => Some(Limited::RightHold),
        Action::SelectHotbarSlot { .. } => Some(Limited::Hotbar),
        Action::SendChat { message } => {
            (!message.as_str().starts_with('/')).then_some(Limited::PlainChat)
        }
        Action::Sneak { on: false } | Action::HoldUse { on: false } => None,
    }
}

/// What the script does differently, by step: each step's own
/// differences, then the first other step whose runs can meet its own
/// under one limit.
fn notes(steps: &[Step], schedules: &[Option<(Duration, Duration)>]) -> Vec<Note> {
    let mut notes = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        let mut note = |kind| notes.push(Note { step: index, kind });
        let schedule = schedules.get(index).copied().flatten();
        if step.action == Action::SwingArm {
            note(NoteKind::SwingSkipsEntities);
        }
        if let (Schedule::Every { interval, .. }, Some((raised, _))) = (step.schedule, schedule)
            && raised > interval
        {
            note(NoteKind::Slower { every: raised });
        }
        if step.action == (Action::HoldUse { on: true }) {
            note(NoteKind::HoldRepeats);
        }
        if let (Schedule::Every { jitter, .. }, Some((_, cut))) = (step.schedule, schedule)
            && cut < jitter
        {
            note(NoteKind::GapCapped);
        }
        if let Some((other, every)) = shared(steps, index) {
            note(NoteKind::SharesLimit { other, every });
        }
    }
    notes
}

/// The first other step whose runs can meet `index`'s under one limit, and
/// that limit's shortest time between two.
///
/// Two steps at start, or two on respawn, run at one moment, and the later
/// one is refused. A step at start and a repeating one never meet: the
/// first run is a whole interval after the join, at least the limit's
/// shortest. A step at start and one on respawn meet only on a respawn
/// right after a join, which isn't noted. Two repeating steps, or a
/// repeating one and one on respawn, can meet whenever.
fn shared(steps: &[Step], index: usize) -> Option<(usize, Duration)> {
    let step = steps.get(index)?;
    let kind = limited(&step.action)?;
    steps
        .iter()
        .enumerate()
        .filter(|(other, _)| *other != index)
        .find(|(other, them)| {
            limited(&them.action) == Some(kind)
                && match (step.schedule, them.schedule) {
                    (Schedule::AtStart, Schedule::AtStart)
                    | (Schedule::OnRespawn, Schedule::OnRespawn) => *other < index,
                    (Schedule::Every { .. }, Schedule::Every { .. } | Schedule::OnRespawn)
                    | (Schedule::OnRespawn, Schedule::Every { .. }) => true,
                    _ => false,
                }
        })
        .map(|(other, _)| (other, kind.minimum()))
}
