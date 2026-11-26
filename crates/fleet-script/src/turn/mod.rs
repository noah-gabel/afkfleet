//! [`Turn`]: what a running hook sees and what it leaves behind, and the
//! rules every action of a script passes (ADR-0036, D4, D7 to D11, M8;
//! ADR-0037, C4). What a hook does in a screen and a sign editor is
//! [`menu`]'s.
//!
//! No Rhai type is named here. The API's functions ([`crate::api`]) turn a
//! script's values into this module's calls and its answers into the
//! results a script reads.

use core::time::Duration;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use fleet_core::chat::ChatMessage;
use fleet_core::mc::MenuView;
use fleet_core::mode::HotbarSlot;
use fleet_core::script::{
    ActionLimiter, ChatWatch, Given, HoldTracker, Ident, Identifier, Input, Key, Leash, Limited,
    LookingAt, MouseButton, Position, Press, Refusal, ScriptApiLevel, SendId, Snapshot, TimerName,
    When, limits, may_mine,
};
use fleet_core::{text, time};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

use crate::action::{Action, Asked, PressKind, Refused, note};
use crate::compile::Program;
use crate::error::Misuse;
use crate::instance::{Moment, Presence};
use crate::timers::{TimerError, Timers};

mod menu;

pub(crate) use menu::{ItemView, Target, kept_lore, number_key};

/// The one kind of vehicle a bot gets into: fleet-mc keeps a bot's seat
/// only in a minecart (ADR-0026).
const MOUNTABLE: &str = "minecraft:minecart";

/// What an action answers: taken, or why not.
pub(crate) type Answer = Result<(), Refusal>;

/// A turn behind its lock: the instance and its engine's functions share
/// it.
pub(crate) type SharedTurn = Arc<Mutex<Turn>>;

/// Locks a turn. Nothing waits here: one hook runs at a time, on one
/// thread. A panic inside a hook leaves the turn as it was; what's there
/// is still used, so the next hook finds its holds and timers.
pub(crate) fn lock(turn: &SharedTurn) -> MutexGuard<'_, Turn> {
    turn.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How long an input is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HoldLength {
    /// Until it's released.
    Open,
    /// For this long; `None` for a length that isn't a number of
    /// milliseconds.
    Time(Option<Duration>),
    /// Until the held item is used up once.
    UntilUsed,
}

/// The values a script gave, as fleet-core takes them; `None` for values
/// that are no map, or a value that is neither a number nor a text.
pub(crate) fn given(values: Option<&[(String, Value)]>) -> Option<Vec<(&str, Given<'_>)>> {
    values?
        .iter()
        .map(|(name, value)| {
            let given = match value {
                Value::Integer(number) => Given::Integer(*number),
                Value::Decimal(number) => Given::Decimal(*number),
                Value::Text(text) => Given::Text(text),
                Value::Other => return None,
            };
            Some((name.as_str(), given))
        })
        .collect()
}

/// A value a script gave for a declared message.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    /// A whole number.
    Integer(i64),
    /// A decimal number.
    Decimal(f64),
    /// A text.
    Text(String),
    /// Anything else: never a value.
    Other,
}

/// When the lines of a script's log were written, for its rate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LogRate {
    /// The lines of the last minute.
    lines: Vec<DateTime<Utc>>,
}

impl LogRate {
    /// How long a line counts.
    const WINDOW: Duration = Duration::from_secs(60);

    /// Whether a line may be written at `now`; if so, it's counted.
    fn take(&mut self, now: DateTime<Utc>) -> bool {
        self.lines
            .retain(|line| time::elapsed(*line, now) < Self::WINDOW);
        let room =
            u32::try_from(self.lines.len()).is_ok_and(|lines| lines < limits::LOG_LINES_PER_MINUTE);
        if room {
            self.lines.push(now);
        }
        room
    }
}

/// What a running hook sees and what it leaves behind.
#[derive(Debug)]
pub(crate) struct Turn {
    /// The script, with the mode's settings and declarations.
    program: Arc<Program>,
    /// What the script holds down.
    pub(crate) holds: HoldTracker,
    /// The script's timers.
    pub(crate) timers: Timers,
    /// The script's random numbers, from its seed.
    rng: StdRng,
    log: LogRate,
    /// The next send's number.
    next_send: u64,
    /// What outlives the hook about screens and the right button.
    lasting: Lasting,

    // The running hook's own, set by `begin`.
    facts: Option<Snapshot>,
    now: DateTime<Utc>,
    presence: Presence,
    leash: Option<Leash>,
    /// The bot's limiter, for the hook's time.
    limiter: ActionLimiter,
    actions: Vec<Action>,
    /// The last declared command this hook sent.
    command: Option<SendId>,
    /// Whether this is `on_stop`: it acts on nothing but one send.
    stopping: bool,
    /// Whether `on_stop` has sent its one message.
    sent_at_stop: bool,
    /// What the hook was refused, for whoever runs the script.
    refused: Vec<Refused>,
    /// The bot's inventory, its screen and its sign editor, as the hook
    /// sees them.
    menu: Option<MenuView>,
    /// The bot's own player name.
    name: String,
    /// What the running hook did to its screen and its sign editor.
    hook: HookScreen,
}

/// What a script's turn keeps between hooks beside its holds and timers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Lasting {
    /// Whether an action the script sent onto a screen waits for its fate
    /// (`on_click`).
    awaiting: bool,
    /// Whether the right button is held until the item is used up once.
    until_used: bool,
}

/// What the running hook did to the screen and the sign editor it was
/// given.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct HookScreen {
    /// It closed the screen.
    closed: bool,
    /// It opened the inventory.
    opened: bool,
    /// It answered the sign editor.
    sign_answered: bool,
}

impl Turn {
    /// A turn for `program`, with nothing held and no timer set.
    pub(crate) fn new(program: Arc<Program>, seed: u64) -> Self {
        Self {
            program,
            holds: HoldTracker::new(),
            timers: Timers::default(),
            rng: StdRng::seed_from_u64(seed),
            log: LogRate::default(),
            next_send: 0,
            lasting: Lasting::default(),
            facts: None,
            now: DateTime::<Utc>::MIN_UTC,
            presence: Presence::NotInWorld,
            leash: None,
            limiter: ActionLimiter::new(),
            actions: Vec::new(),
            command: None,
            stopping: false,
            sent_at_stop: false,
            refused: Vec::new(),
            menu: None,
            name: String::new(),
            hook: HookScreen::default(),
        }
    }

    /// Starts a hook at `moment` with the bot's `limiter`.
    pub(crate) fn begin(&mut self, moment: &Moment<'_>, limiter: ActionLimiter, stopping: bool) {
        self.facts = Some(moment.facts.clone());
        self.menu = Some(moment.menu.clone());
        moment.name.clone_into(&mut self.name);
        self.hook = HookScreen::default();
        self.now = moment.now;
        self.presence = moment.presence;
        self.leash = moment.leash;
        self.limiter = limiter;
        self.actions.clear();
        self.command = None;
        self.stopping = stopping;
        self.sent_at_stop = false;
        self.refused.clear();
    }

    /// Ends the hook: what it asked for, the limiter back, and what it
    /// was refused.
    pub(crate) fn end(&mut self) -> (Vec<Action>, ActionLimiter, Vec<Refused>) {
        self.facts = None;
        self.menu = None;
        self.command = None;
        self.stopping = false;
        (
            core::mem::take(&mut self.actions),
            core::mem::take(&mut self.limiter),
            core::mem::take(&mut self.refused),
        )
    }

    /// Notes a refusal of `asked` for whoever runs the script, and hands
    /// the answer on to the script.
    fn noted(&mut self, asked: Asked, answer: Answer) -> Answer {
        if let Err(reason) = answer {
            note(&mut self.refused, asked, reason, 1);
        }
        answer
    }

    /// The facts of the running hook.
    pub(crate) const fn facts(&self) -> Option<&Snapshot> {
        self.facts.as_ref()
    }

    /// The API level the script was validated at.
    pub(crate) fn level(&self) -> ScriptApiLevel {
        self.program.script().api()
    }

    /// The bot's own player name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// The running hook's moment.
    pub(crate) const fn now(&self) -> DateTime<Utc> {
        self.now
    }

    /// Whether a key, a button, a look, the hotbar or a vehicle may act:
    /// not while a screen is open (ADR-0036, M8).
    fn free_hands(&self) -> Answer {
        if self.screen_open() {
            return Err(Refusal::ScreenOpen);
        }
        Ok(())
    }

    /// Whether the bot can act at all right now.
    const fn can_act(&self) -> Answer {
        if self.stopping {
            return Err(Refusal::NotInWorld);
        }
        match self.presence {
            Presence::InWorld => Ok(()),
            Presence::Dead => Err(Refusal::Dead),
            Presence::NotInWorld => Err(Refusal::NotInWorld),
        }
    }

    /// Takes an action of kind `what` now, or says it's too soon.
    fn limit(&mut self, what: Limited) -> Answer {
        self.limiter
            .try_act(what, self.now)
            .map_err(|_| Refusal::TooSoon)
    }

    /// Whether pressing `input` would walk the bot further beyond its
    /// leash, with what it already holds (ADR-0036, D10).
    fn leashed(&self, input: Input) -> bool {
        let (Some(leash), Some(facts), Input::Key(key)) = (self.leash, &self.facts, input) else {
            return false;
        };
        let held = self.holds.held().filter_map(|held| match held {
            Input::Key(key) => Some(key),
            Input::Mouse(_) => None,
        });
        let bot = facts.bot();
        matches!(key, Key::Forward | Key::Back | Key::Left | Key::Right)
            && leash.would_go_further(bot.position, bot.yaw, held.chain([key]))
    }

    /// Why a hold of the left button may not start: a block that reaches
    /// below the bot's feet, a tool that's nearly used up, or a block the
    /// mode doesn't list, judged in that order (ADR-0036, D11; ADR-0042's
    /// amendment, G13).
    ///
    /// A bot that looks at no block may start: the session below pauses
    /// the hold at every block a guard holds back.
    fn guarded(&self) -> Answer {
        let Some(facts) = &self.facts else {
            return Ok(());
        };
        let bot = facts.bot();
        let script = self.program.script();
        let block = match &bot.looking_at {
            Some(LookingAt::Block { id, at }) => Some((id, *at)),
            Some(LookingAt::Entity { .. }) | None => None,
        };
        if block.is_some_and(|(_, at)| !may_mine(bot.position.y(), at)) {
            return Err(Refusal::MiningGuard);
        }
        if !script.tool_guard().allows(bot.held.as_ref()) {
            return Err(Refusal::ToolGuard);
        }
        if block.is_some_and(|(id, _)| !script.block_guard().allows(Some(id))) {
            return Err(Refusal::BlockGuard);
        }
        Ok(())
    }

    /// Presses `input` and lets go at once.
    pub(crate) fn tap(&mut self, input: Input) -> Answer {
        let answer = self.try_tap(input);
        self.noted(Asked::Press, answer)
    }

    /// [`tap`](Self::tap), before its refusal is noted.
    fn try_tap(&mut self, input: Input) -> Answer {
        self.can_act()?;
        self.free_hands()?;
        if self.leashed(input) {
            return Err(Refusal::Leashed);
        }
        self.limit(Limited::of_press(input, Press::Tap))?;
        // A tap of an input that is down lets go of it.
        self.holds.release(input);
        self.actions.push(Action::Press {
            input,
            kind: PressKind::Tap,
        });
        Ok(())
    }

    /// Holds `input` down.
    ///
    /// # Errors
    /// [`Misuse`] for a key that only taps, and for `UntilUsed` on
    /// anything but the right button: mistakes no value excuses.
    pub(crate) fn hold(&mut self, input: Input, length: HoldLength) -> Result<Answer, Misuse> {
        if !input.can_be_held() {
            return Err(Misuse::TapOnly);
        }
        if length == HoldLength::UntilUsed && input != Input::Mouse(MouseButton::Right) {
            return Err(Misuse::RightButtonOnly);
        }
        let answer = self.held(input, length);
        Ok(self.noted(Asked::Press, answer))
    }

    /// [`hold`](Self::hold), for an input that can be held that way.
    fn held(&mut self, input: Input, length: HoldLength) -> Answer {
        let (time, kind) = match length {
            HoldLength::Time(Some(time))
                if (limits::MIN_HOLD_FOR..=limits::MAX_HOLD_FOR).contains(&time) =>
            {
                (Some(time), PressKind::Hold)
            }
            HoldLength::Time(_) => return Err(Refusal::BadValue),
            HoldLength::Open => (None, PressKind::Hold),
            HoldLength::UntilUsed => (None, PressKind::HoldUntilUsed),
        };
        self.can_act()?;
        self.free_hands()?;
        // Down already, a hold presses nothing again: it only sets how
        // long the input stays down.
        if !self.holds.is_held(input) {
            if self.leashed(input) {
                return Err(Refusal::Leashed);
            }
            if input == Input::Mouse(MouseButton::Left) {
                self.guarded()?;
            }
            self.limit(Limited::of_press(input, Press::Hold))?;
            self.actions.push(Action::Press { input, kind });
        }
        if input == Input::Mouse(MouseButton::Right) {
            self.lasting.until_used = kind == PressKind::HoldUntilUsed;
        }
        // The checks above rule out both of the tracker's errors.
        let pressed = match time {
            Some(time) => self.holds.hold_for(input, time, self.now),
            None => self.holds.hold(input),
        };
        pressed.map(|_| ()).map_err(|_| Refusal::BadValue)
    }

    /// Lets go of `input`. Never refused, whatever the bot's state: the
    /// release is asked for even if the script held nothing, since what is
    /// down below is the session's to know.
    pub(crate) fn release(&mut self, input: Input) {
        self.holds.release(input);
        if input == Input::Mouse(MouseButton::Right) {
            self.lasting.until_used = false;
        }
        self.actions.push(Action::Release(input));
    }

    /// Lets go of every key, not of the buttons. Never refused.
    pub(crate) fn release_keys(&mut self) {
        self.holds.release_keys();
        self.actions.push(Action::ReleaseKeys);
    }

    /// Whether the script holds `input` down.
    pub(crate) fn is_held(&self, input: Input) -> bool {
        self.holds.is_held(input)
    }

    /// Looks in a direction; `None` for an angle that is no number.
    pub(crate) fn look_at(&mut self, yaw: Option<f64>, pitch: Option<f64>) -> Answer {
        let answer = self.try_look_at(yaw, pitch);
        self.noted(Asked::Look, answer)
    }

    /// [`look_at`](Self::look_at), before its refusal is noted.
    fn try_look_at(&mut self, yaw: Option<f64>, pitch: Option<f64>) -> Answer {
        let (Some(yaw), Some(pitch)) = (yaw, pitch) else {
            return Err(Refusal::BadValue);
        };
        if !(-180.0..=180.0).contains(&yaw) || !(-90.0..=90.0).contains(&pitch) {
            return Err(Refusal::BadValue);
        }
        self.can_act()?;
        self.free_hands()?;
        self.limit(Limited::LookAt)?;
        self.actions.push(Action::LookAt { yaw, pitch });
        Ok(())
    }

    /// Turns by an amount; `None` for an angle that is no number.
    pub(crate) fn look_turn(&mut self, yaw: Option<f64>, pitch: Option<f64>) -> Answer {
        let answer = self.try_look_turn(yaw, pitch);
        self.noted(Asked::Look, answer)
    }

    /// [`look_turn`](Self::look_turn), before its refusal is noted.
    fn try_look_turn(&mut self, yaw: Option<f64>, pitch: Option<f64>) -> Answer {
        let (Some(yaw), Some(pitch)) = (yaw, pitch) else {
            return Err(Refusal::BadValue);
        };
        self.can_act()?;
        self.free_hands()?;
        self.limit(Limited::LookTurn)?;
        self.actions.push(Action::LookTurn { yaw, pitch });
        Ok(())
    }

    /// Looks at a point; `None` for coordinates that are no numbers.
    pub(crate) fn look_at_position(&mut self, point: Option<Position>) -> Answer {
        let answer = self.try_look_at_position(point);
        self.noted(Asked::Look, answer)
    }

    /// [`look_at_position`](Self::look_at_position), before its refusal is noted.
    fn try_look_at_position(&mut self, point: Option<Position>) -> Answer {
        let point = point.ok_or(Refusal::BadValue)?;
        self.can_act()?;
        self.free_hands()?;
        self.limit(Limited::LookAt)?;
        self.actions.push(Action::LookAtPosition(point));
        Ok(())
    }

    /// Selects a hotbar slot; `None` for a number that is not 1 to 9.
    pub(crate) fn select(&mut self, slot: Option<HotbarSlot>) -> Answer {
        let answer = self.try_select(slot);
        self.noted(Asked::Hotbar, answer)
    }

    /// [`select`](Self::select), before its refusal is noted.
    fn try_select(&mut self, slot: Option<HotbarSlot>) -> Answer {
        let slot = slot.ok_or(Refusal::BadValue)?;
        self.can_act()?;
        self.free_hands()?;
        self.limit(Limited::Hotbar)?;
        self.actions.push(Action::SelectHotbar(slot));
        Ok(())
    }

    /// Gets into the nearest free vehicle of `kind`; `None` for text that
    /// names no kind.
    pub(crate) fn mount(&mut self, kind: Option<Identifier>) -> Answer {
        let answer = self.try_mount(kind);
        self.noted(Asked::Vehicle, answer)
    }

    /// [`mount`](Self::mount), before its refusal is noted.
    fn try_mount(&mut self, kind: Option<Identifier>) -> Answer {
        let kind = kind
            .filter(|kind| kind.as_str() == MOUNTABLE)
            .ok_or(Refusal::BadValue)?;
        self.can_act()?;
        self.free_hands()?;
        self.limit(Limited::Mount)?;
        self.actions.push(Action::Mount(kind));
        Ok(())
    }

    /// Gets out of the vehicle.
    pub(crate) fn dismount(&mut self) -> Answer {
        let answer = self.try_dismount();
        self.noted(Asked::Vehicle, answer)
    }

    /// [`dismount`](Self::dismount), before its refusal is noted.
    fn try_dismount(&mut self) -> Answer {
        self.can_act()?;
        self.free_hands()?;
        self.limit(Limited::Dismount)?;
        self.actions.push(Action::Dismount);
        Ok(())
    }

    /// Sends the declared message `key` with `values`; `None` for a key
    /// that is no text, and for values that are no map.
    ///
    /// # Errors
    /// [`Misuse::UnknownMessage`] for a key the mode does not declare.
    pub(crate) fn send(
        &mut self,
        key: Option<&str>,
        values: Option<&[(String, Value)]>,
    ) -> Result<Answer, Misuse> {
        let answer = self.try_send(key, values)?;
        Ok(self.noted(Asked::Send, answer))
    }

    /// [`send`](Self::send), before its refusal is noted.
    fn try_send(
        &mut self,
        key: Option<&str>,
        values: Option<&[(String, Value)]>,
    ) -> Result<Answer, Misuse> {
        let program = Arc::clone(&self.program);
        let template = key
            .and_then(|key| program.script().message(key))
            .ok_or(Misuse::UnknownMessage)?;
        let Some(given) = given(values) else {
            return Ok(Err(Refusal::BadValue));
        };
        // `on_stop` may send one message, whatever else the bot can do.
        if self.stopping {
            if self.sent_at_stop {
                return Ok(Err(Refusal::NotInWorld));
            }
        } else if let Err(refusal) = self.can_act() {
            return Ok(Err(refusal));
        }
        let Ok(message) = template.render(&given) else {
            return Ok(Err(Refusal::BadValue));
        };
        Ok(self.sent(template.key().clone(), message))
    }

    /// Hands a finished message on, past the limit for plain chat.
    fn sent(&mut self, key: Ident, message: ChatMessage) -> Answer {
        let command = message.is_command();
        // A declared command goes by the bot's own chat rate, below this
        // crate; plain chat keeps a step mode's 30 s (ADR-0036, D7).
        if !command {
            self.limit(Limited::PlainChat)?;
        }
        let id = SendId(self.next_send);
        self.next_send = self.next_send.saturating_add(1);
        if command {
            self.command = Some(id);
        }
        self.sent_at_stop = self.stopping;
        self.actions.push(Action::Send { id, key, message });
        Ok(())
    }

    /// Asks for a window of `window` for the reply pattern `key`, to open
    /// when the last declared command this hook sent is handed to the
    /// server; `None` for a length that is no number of milliseconds.
    ///
    /// # Errors
    /// [`Misuse::UnknownPattern`] for a key that is no declared
    /// pattern's, and [`Misuse::NotAReplyPattern`] for one declared to
    /// come at any time.
    pub(crate) fn expect(
        &mut self,
        key: Option<&str>,
        window: Option<Duration>,
    ) -> Result<Answer, Misuse> {
        let answer = self.try_expect(key, window)?;
        Ok(self.noted(Asked::Expect, answer))
    }

    /// [`expect`](Self::expect), before its refusal is noted.
    fn try_expect(
        &mut self,
        key: Option<&str>,
        window: Option<Duration>,
    ) -> Result<Answer, Misuse> {
        let program = Arc::clone(&self.program);
        let pattern = key
            .and_then(|key| program.script().patterns().get(key))
            .ok_or(Misuse::UnknownPattern)?;
        if pattern.when() != When::Reply {
            return Err(Misuse::NotAReplyPattern);
        }
        let Some(window) = window
            .filter(|window| (ChatWatch::MIN_WINDOW..=ChatWatch::MAX_WINDOW).contains(window))
        else {
            return Ok(Err(Refusal::BadValue));
        };
        if let Err(refusal) = self.can_act() {
            return Ok(Err(refusal));
        }
        // The window belongs to a command of this hook's own (ADR-0037,
        // C4).
        let Some(send) = self.command else {
            return Ok(Err(Refusal::NoCommand));
        };
        self.actions.push(Action::Expect {
            send,
            pattern: pattern.key().clone(),
            window,
        });
        Ok(Ok(()))
    }

    /// Sets the timer `name` to run `length` from now; `None` for a
    /// length or a name that is none.
    pub(crate) fn timer(
        &mut self,
        length: Option<Duration>,
        name: Option<TimerName>,
        repeats: bool,
    ) -> Answer {
        let answer = self.try_timer(length, name, repeats);
        self.noted(Asked::Timer, answer)
    }

    /// [`timer`](Self::timer), before its refusal is noted.
    fn try_timer(
        &mut self,
        length: Option<Duration>,
        name: Option<TimerName>,
        repeats: bool,
    ) -> Answer {
        let (Some(length), Some(name)) = (length, name) else {
            return Err(Refusal::BadValue);
        };
        if self.stopping {
            return Err(Refusal::NotInWorld);
        }
        self.timers
            .set(name, length, repeats, self.now)
            .map_err(|error| match error {
                TimerError::Length => Refusal::BadValue,
                TimerError::TooMany => Refusal::TooMany,
            })
    }

    /// A whole number from `low` to `high`, both included, whichever is
    /// the lower.
    pub(crate) fn random_between(&mut self, low: i64, high: i64) -> i64 {
        self.rng.random_range(low.min(high)..=low.max(high))
    }

    /// `true` in `percent` of a hundred cases: never at 0 or below, always
    /// at 100 or above, never for something that is no number.
    pub(crate) fn random_chance(&mut self, percent: f64) -> bool {
        self.rng.random_range(0.0..100.0) < percent
    }

    /// Holds again, as the script held them, the inputs a screen made the
    /// session let go of: `holds` with when each was to be let go by
    /// itself, and whether the right button was held until used. What the
    /// limiter, the leash or a guard refuses now stays up, and is noted
    /// (ADR-0052: a level-1 script has no hook that would hear of it).
    pub(crate) fn press_again(
        &mut self,
        holds: &[(Input, Option<DateTime<Utc>>)],
        until_used: bool,
    ) {
        for (input, until) in holds {
            let length = match until {
                None if *input == Input::Mouse(MouseButton::Right) && until_used => {
                    HoldLength::UntilUsed
                }
                None => HoldLength::Open,
                // What was left of a `hold_for`; past its end, nothing.
                Some(until) => match until.signed_duration_since(self.now).to_std() {
                    Ok(left) if left >= limits::MIN_HOLD_FOR => HoldLength::Time(Some(left)),
                    Ok(_) | Err(_) => continue,
                },
            };
            // Every input here was held, so it can be held that way.
            let _ = self.hold(*input, length);
        }
    }

    /// Whether the right button was held until the item is used up once.
    pub(crate) const fn held_until_used(&self) -> bool {
        self.lasting.until_used
    }

    /// Writes a line into the log, one line of at most
    /// [`limits::LOG_LINE_CHARS`] characters; a longer one is cut.
    pub(crate) fn log(&mut self, line: &str) -> Answer {
        let answer = self.try_log(line);
        self.noted(Asked::Log, answer)
    }

    /// [`log`](Self::log), before its refusal is noted.
    fn try_log(&mut self, line: &str) -> Answer {
        if !self.log.take(self.now) {
            return Err(Refusal::TooSoon);
        }
        let line = text::sanitize_untrusted(line, limits::LOG_LINE_CHARS).text;
        self.actions.push(Action::Log(line));
        Ok(())
    }
}
