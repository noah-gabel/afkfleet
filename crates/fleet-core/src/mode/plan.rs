//! [`ModePlan`]: when a mode's steps run, and what exactly they do.

use core::time::Duration;

use chrono::{DateTime, Utc};
use rand::{Rng, RngExt};

use super::{Action, HotbarSlot, ModeDefinition, Schedule};
use crate::chat::ChatMessage;
use crate::time;

/// A game action resolved for the session to perform: random choices are
/// made, and chat is split off into [`PlannedAction::Chat`].
///
/// P2.10's `SessionHandle::perform` takes it (ADR-0010).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GameAction {
    /// Look in a fixed direction, in degrees.
    Look {
        /// −180 to 180.
        yaw: f32,
        /// −90 (straight up) to 90 (straight down).
        pitch: f32,
    },
    /// Turn by this much from the current direction, in degrees. The
    /// adapter clamps the resulting pitch to −90..=90 (P3.6).
    Turn {
        /// Positive turns right, negative left.
        yaw: f32,
        /// Positive turns down, negative up.
        pitch: f32,
    },
    /// Jump once.
    Jump,
    /// Start or stop sneaking.
    Sneak {
        /// Whether the bot sneaks from now on.
        on: bool,
    },
    /// Swing the main arm.
    SwingArm,
    /// Use the held item, like a right-click.
    UseItem,
    /// Attack the entity the bot is looking at, if one is within reach.
    AttackFacingEntity,
    /// Select a hotbar slot.
    SelectHotbarSlot {
        /// The slot.
        slot: HotbarSlot,
    },
}

/// One thing the runtime does for a mode: a game action, or a chat message
/// for the bot's outbound chat queue (P4.5).
#[derive(Debug, Clone, PartialEq)]
pub enum PlannedAction {
    /// Perform this on the session.
    Game(GameAction),
    /// Send this through the chat queue.
    Chat(ChatMessage),
}

/// What [`ModePlan::start`] and [`ModePlan::tick`] return: what to do now,
/// and when to tick next.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanTick {
    /// The actions to run now, in step order.
    pub actions: Vec<PlannedAction>,
    /// When the next step is due, or `None` if no step repeats.
    pub next_due: Option<DateTime<Utc>>,
}

/// Schedules a running mode: what to do when, so the runtime only sleeps
/// until [`PlanTick::next_due`] and executes the actions (Plan.md P2.8;
/// ADR-0010).
///
/// - [`ModePlan::start`] runs the [`Schedule::AtStart`] steps and schedules
///   each [`Schedule::Every`] step one gap after the start.
/// - [`ModePlan::tick`] runs every step that is due and schedules it again,
///   one gap after `now`. Each gap is uniform in [`interval`,
///   `interval + jitter`].
/// - A step that's due rolls its probability first; a skipped run is
///   scheduled again like any other.
///
/// Scheduling from `now` instead of from the due time means there's no
/// catch-up after a late tick, and two runs of a step are never closer than
/// its interval, so the attack and chat limits hold even when the runtime
/// wakes up late. The plan holds no random number generator: the caller
/// passes one in.
#[derive(Debug, Clone, PartialEq)]
pub struct ModePlan {
    timers: Vec<Timer>,
}

/// A repeating step and when it's due next.
#[derive(Debug, Clone, PartialEq)]
struct Timer {
    action: Action,
    probability: u8,
    interval: Duration,
    jitter: Duration,
    due: DateTime<Utc>,
}

impl ModePlan {
    /// Starts running `definition` at `now`: returns the plan, and a tick
    /// with the at-start actions and the first due time.
    pub fn start<R: Rng + ?Sized>(
        definition: &ModeDefinition,
        now: DateTime<Utc>,
        rng: &mut R,
    ) -> (Self, PlanTick) {
        let mut actions = Vec::new();
        let mut timers = Vec::new();
        for step in definition.steps() {
            match step.schedule {
                Schedule::AtStart => {
                    if roll(step.probability, rng) {
                        actions.push(resolve(&step.action, rng));
                    }
                }
                Schedule::Every { interval, jitter } => timers.push(Timer {
                    action: step.action.clone(),
                    probability: step.probability,
                    interval,
                    jitter,
                    due: time::add(now, gap(interval, jitter, rng)),
                }),
            }
        }
        let plan = Self { timers };
        let tick = PlanTick {
            actions,
            next_due: plan.next_due(),
        };
        (plan, tick)
    }

    /// Runs every step that is due at `now`, in step order, and schedules
    /// each one again. Before the next due time it runs nothing, and that
    /// includes a clock that went backwards.
    pub fn tick<R: Rng + ?Sized>(&mut self, now: DateTime<Utc>, rng: &mut R) -> PlanTick {
        let mut actions = Vec::new();
        for timer in self.timers.iter_mut().filter(|timer| timer.due <= now) {
            if roll(timer.probability, rng) {
                actions.push(resolve(&timer.action, rng));
            }
            timer.due = time::add(now, gap(timer.interval, timer.jitter, rng));
        }
        PlanTick {
            actions,
            next_due: self.next_due(),
        }
    }

    /// When the next step is due, or `None` if no step repeats.
    #[must_use]
    pub fn next_due(&self) -> Option<DateTime<Utc>> {
        self.timers.iter().map(|timer| timer.due).min()
    }
}

/// Whether a step with this probability (a percent) runs this time.
fn roll<R: Rng + ?Sized>(probability: u8, rng: &mut R) -> bool {
    // A validated probability is at most 100; the clamp keeps
    // `random_ratio` from panicking even so (ADR-0010).
    rng.random_ratio(u32::from(probability).min(100), 100)
}

/// A gap uniform in [`interval`, `interval + jitter`].
fn gap<R: Rng + ?Sized>(interval: Duration, jitter: Duration, rng: &mut R) -> Duration {
    // Never empty: the upper end is at least `interval`.
    rng.random_range(interval..=interval.saturating_add(jitter))
}

/// A random angle in `-max..=max`, or 0 for a maximum of 0.
fn symmetric<R: Rng + ?Sized>(max: f32, rng: &mut R) -> f32 {
    // A validated maximum is finite and at least 0; anything else is never
    // sampled, because `random_range` would panic (ADR-0010).
    if max.is_finite() && max > 0.0 {
        rng.random_range(-max..=max)
    } else {
        0.0
    }
}

/// Makes the random choices of `action` and splits off chat.
fn resolve<R: Rng + ?Sized>(action: &Action, rng: &mut R) -> PlannedAction {
    let game = match *action {
        Action::Look { yaw, pitch } => GameAction::Look { yaw, pitch },
        Action::RotateRandom { max_yaw, max_pitch } => GameAction::Turn {
            yaw: symmetric(max_yaw, rng),
            pitch: symmetric(max_pitch, rng),
        },
        Action::Jump => GameAction::Jump,
        Action::Sneak { on } => GameAction::Sneak { on },
        Action::SwingArm => GameAction::SwingArm,
        Action::UseItem => GameAction::UseItem,
        Action::AttackFacingEntity => GameAction::AttackFacingEntity,
        Action::SelectHotbarSlot { slot } => GameAction::SelectHotbarSlot { slot },
        Action::SendChat { ref message } => return PlannedAction::Chat(message.clone()),
    };
    PlannedAction::Game(game)
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use proptest::collection::vec;
    use proptest::prelude::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use rstest::rstest;

    use super::*;
    use crate::mode::{ModeDraft, Step};
    use crate::time;

    /// The mode starts here in every test: far from both ends of
    /// `DateTime`, so saturation never matters.
    const START_SECS: i64 = 1_700_000_000;

    fn start() -> DateTime<Utc> {
        DateTime::from_timestamp(START_SECS, 0).unwrap()
    }

    /// `ms` milliseconds after the start.
    fn after(ms: u64) -> DateTime<Utc> {
        time::add(start(), Duration::from_millis(ms))
    }

    const fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    const fn every(interval: Duration, jitter: Duration) -> Schedule {
        Schedule::Every { interval, jitter }
    }

    fn step(action: Action, schedule: Schedule) -> Step {
        Step {
            action,
            schedule,
            probability: 100,
        }
    }

    fn at_start(action: Action) -> Step {
        step(action, Schedule::AtStart)
    }

    fn with_probability(probability: u8, step: Step) -> Step {
        Step {
            probability,
            ..step
        }
    }

    fn slot(n: u8) -> HotbarSlot {
        HotbarSlot::try_from(n).unwrap()
    }

    fn definition(steps: Vec<Step>) -> ModeDefinition {
        ModeDraft { steps }.validate().unwrap()
    }

    fn rng(seed: u64) -> StdRng {
        StdRng::seed_from_u64(seed)
    }

    const fn game(action: GameAction) -> PlannedAction {
        PlannedAction::Game(action)
    }

    #[test]
    fn an_empty_mode_does_nothing() {
        let mut rng = rng(1);
        let (mut plan, first) = ModePlan::start(&definition(Vec::new()), start(), &mut rng);

        assert_eq!(
            first,
            PlanTick {
                actions: Vec::new(),
                next_due: None
            }
        );
        assert_eq!(plan.next_due(), None);
        assert_eq!(
            plan.tick(after(60_000), &mut rng),
            PlanTick {
                actions: Vec::new(),
                next_due: None
            }
        );
    }

    #[test]
    fn at_start_steps_run_once_when_the_mode_starts() {
        let mut rng = rng(1);
        let mode = definition(vec![
            at_start(Action::Jump),
            at_start(Action::Sneak { on: true }),
        ]);

        let (mut plan, first) = ModePlan::start(&mode, start(), &mut rng);
        assert_eq!(
            first,
            PlanTick {
                actions: vec![game(GameAction::Jump), game(GameAction::Sneak { on: true })],
                next_due: None,
            }
        );
        assert_eq!(
            plan.tick(after(3_600_000), &mut rng),
            PlanTick {
                actions: Vec::new(),
                next_due: None
            }
        );
    }

    #[test]
    fn the_first_run_comes_one_gap_after_the_start() {
        let mode = definition(vec![step(Action::SwingArm, every(ms(20_000), ms(20_000)))]);

        let dues: Vec<_> = (0..50)
            .map(|seed| {
                let (plan, first) = ModePlan::start(&mode, start(), &mut rng(seed));
                assert_eq!(first.actions, Vec::new());
                assert_eq!(plan.next_due(), first.next_due);
                first.next_due.unwrap()
            })
            .collect();
        assert!(
            dues.iter()
                .all(|due| (after(20_000)..=after(40_000)).contains(due)),
            "{dues:?}"
        );
        assert!(
            dues.iter().any(|due| *due != dues[0]),
            "no jitter in {dues:?}"
        );
    }

    #[test]
    fn a_step_without_jitter_runs_exactly_every_interval() {
        let mut rng = rng(1);
        let mode = definition(vec![step(Action::Jump, every(ms(250), Duration::ZERO))]);

        let (mut plan, first) = ModePlan::start(&mode, start(), &mut rng);
        assert_eq!(first.next_due, Some(after(250)));
        for n in 1..=4 {
            assert_eq!(
                plan.tick(after(250 * n), &mut rng),
                PlanTick {
                    actions: vec![game(GameAction::Jump)],
                    next_due: Some(after(250 * (n + 1))),
                }
            );
        }
    }

    #[test]
    fn a_tick_runs_every_due_step_in_step_order() {
        let mut rng = rng(1);
        let mode = definition(vec![
            step(Action::SwingArm, every(ms(10_000), Duration::ZERO)),
            step(Action::Jump, every(ms(1_000), Duration::ZERO)),
        ]);

        let (mut plan, first) = ModePlan::start(&mode, start(), &mut rng);
        assert_eq!(first.next_due, Some(after(1_000)));
        for n in 1..10 {
            assert_eq!(
                plan.tick(after(1_000 * n), &mut rng),
                PlanTick {
                    actions: vec![game(GameAction::Jump)],
                    next_due: Some(after(1_000 * (n + 1))),
                }
            );
        }
        assert_eq!(
            plan.tick(after(10_000), &mut rng),
            PlanTick {
                actions: vec![game(GameAction::SwingArm), game(GameAction::Jump)],
                next_due: Some(after(11_000)),
            }
        );
    }

    #[rstest]
    #[case::early(after(999))]
    #[case::at_the_start(start())]
    #[case::clock_went_backwards(time::add(DateTime::<Utc>::MIN_UTC, Duration::from_secs(1)))]
    fn a_tick_before_the_next_due_time_does_nothing(#[case] now: DateTime<Utc>) {
        let mut rng = rng(1);
        let mode = definition(vec![step(Action::Jump, every(ms(1_000), Duration::ZERO))]);
        let (mut plan, _) = ModePlan::start(&mode, start(), &mut rng);

        assert_eq!(
            plan.tick(now, &mut rng),
            PlanTick {
                actions: Vec::new(),
                next_due: Some(after(1_000))
            }
        );
        assert_eq!(plan.next_due(), Some(after(1_000)));
    }

    #[test]
    fn a_late_tick_runs_each_step_once_and_reschedules_from_now() {
        let mut rng = rng(1);
        let mode = definition(vec![step(Action::Jump, every(ms(1_000), Duration::ZERO))]);
        let (mut plan, _) = ModePlan::start(&mode, start(), &mut rng);

        assert_eq!(
            plan.tick(after(10_500), &mut rng),
            PlanTick {
                actions: vec![game(GameAction::Jump)],
                next_due: Some(after(11_500)),
            }
        );
    }

    #[test]
    fn a_skipped_run_is_still_rescheduled() {
        let mut rng = rng(3);
        let mode = definition(vec![with_probability(
            1,
            step(Action::Jump, every(ms(1_000), Duration::ZERO)),
        )]);
        let (mut plan, _) = ModePlan::start(&mode, start(), &mut rng);

        let mut skipped = 0;
        for n in 1..=100 {
            let tick = plan.tick(after(1_000 * n), &mut rng);
            if tick.actions.is_empty() {
                skipped += 1;
            }
            assert_eq!(tick.next_due, Some(after(1_000 * (n + 1))));
        }
        assert!(skipped > 0, "no run was skipped at 1 %");
    }

    #[test]
    fn repeating_steps_run_about_as_often_as_their_probability() {
        let mut rng = rng(5);
        let mode = definition(vec![with_probability(
            25,
            step(Action::Jump, every(ms(250), Duration::ZERO)),
        )]);
        let (mut plan, _) = ModePlan::start(&mode, start(), &mut rng);

        let runs: usize = (1..=10_000)
            .map(|n| plan.tick(after(250 * n), &mut rng).actions.len())
            .sum();
        assert!((2_250..=2_750).contains(&runs), "{runs} runs out of 10 000");
    }

    #[test]
    fn at_start_steps_run_about_as_often_as_their_probability() {
        let mut rng = rng(5);
        let mode = definition(vec![with_probability(25, at_start(Action::Jump))]);

        let runs: usize = (0..10_000)
            .map(|_| ModePlan::start(&mode, start(), &mut rng).1.actions.len())
            .sum();
        assert!((2_250..=2_750).contains(&runs), "{runs} runs out of 10 000");
    }

    #[rstest]
    #[case::look(Action::Look { yaw: -45.5, pitch: 30.0 }, game(GameAction::Look { yaw: -45.5, pitch: 30.0 }))]
    #[case::jump(Action::Jump, game(GameAction::Jump))]
    #[case::sneak(Action::Sneak { on: false }, game(GameAction::Sneak { on: false }))]
    #[case::swing_arm(Action::SwingArm, game(GameAction::SwingArm))]
    #[case::use_item(Action::UseItem, game(GameAction::UseItem))]
    #[case::attack(Action::AttackFacingEntity, game(GameAction::AttackFacingEntity))]
    #[case::select_hotbar_slot(
        Action::SelectHotbarSlot { slot: slot(4) },
        game(GameAction::SelectHotbarSlot { slot: slot(4) })
    )]
    #[case::send_chat(
        Action::SendChat { message: "/spawn".parse().unwrap() },
        PlannedAction::Chat("/spawn".parse().unwrap())
    )]
    fn resolves_each_action(#[case] action: Action, #[case] expected: PlannedAction) {
        let (_, first) = ModePlan::start(&definition(vec![at_start(action)]), start(), &mut rng(1));
        assert_eq!(first.actions, [expected]);
    }

    /// The turn of a single at-start `RotateRandom`.
    fn turn(max_yaw: f32, max_pitch: f32, seed: u64) -> (f32, f32) {
        let mode = definition(vec![at_start(Action::RotateRandom { max_yaw, max_pitch })]);
        let (_, first) = ModePlan::start(&mode, start(), &mut rng(seed));
        match first.actions.as_slice() {
            [PlannedAction::Game(GameAction::Turn { yaw, pitch })] => (*yaw, *pitch),
            other => panic!("expected one turn, got {other:?}"),
        }
    }

    #[test]
    fn rotate_random_becomes_a_turn_within_its_maxima_both_ways() {
        let turns: Vec<_> = (0..200).map(|seed| turn(30.0, 10.0, seed)).collect();

        assert!(
            turns
                .iter()
                .all(|(yaw, pitch)| yaw.abs() <= 30.0 && pitch.abs() <= 10.0),
            "{turns:?}"
        );
        assert!(turns.iter().any(|(yaw, _)| *yaw < -15.0), "never far left");
        assert!(turns.iter().any(|(yaw, _)| *yaw > 15.0), "never far right");
        assert!(turns.iter().any(|(_, pitch)| *pitch < -5.0), "never far up");
        assert!(
            turns.iter().any(|(_, pitch)| *pitch > 5.0),
            "never far down"
        );
    }

    #[test]
    fn a_turn_with_one_zero_maximum_stays_on_that_axis() {
        for seed in 0..50 {
            let (yaw, pitch) = turn(30.0, 0.0, seed);
            assert!(yaw.abs() <= 30.0 && pitch == 0.0, "({yaw}, {pitch})");
            let (yaw, pitch) = turn(0.0, 10.0, seed);
            assert!(yaw == 0.0 && pitch.abs() <= 10.0, "({yaw}, {pitch})");
        }
    }

    #[test]
    fn the_presets_start_on_their_documented_schedule() {
        for seed in 0..50 {
            let (_, afk) = ModePlan::start(&ModeDefinition::afk(), start(), &mut rng(seed));
            assert_eq!(afk.actions, Vec::new());
            // The swing comes first: 20–40 s, before the turn's 45–120 s.
            let due = afk.next_due.unwrap();
            assert!((after(20_000)..=after(40_000)).contains(&due), "{due}");

            let (_, farm) = ModePlan::start(&ModeDefinition::farm(), start(), &mut rng(seed));
            assert_eq!(
                farm.actions,
                [game(GameAction::SelectHotbarSlot {
                    slot: HotbarSlot::FIRST
                })]
            );
            let due = farm.next_due.unwrap();
            assert!((after(650)..=after(800)).contains(&due), "{due}");
        }
    }

    /// Runs `mode` for `ticks` ticks, each at the next due time.
    fn run(mode: &ModeDefinition, seed: u64, ticks: usize) -> Vec<PlanTick> {
        let mut rng = rng(seed);
        let (mut plan, first) = ModePlan::start(mode, start(), &mut rng);
        let mut due = first.next_due;
        let mut log = vec![first];
        for _ in 0..ticks {
            let tick = plan.tick(due.unwrap(), &mut rng);
            due = tick.next_due;
            log.push(tick);
        }
        log
    }

    #[rstest]
    #[case::afk(ModeDefinition::afk())]
    #[case::farm(ModeDefinition::farm())]
    fn the_same_seed_gives_the_same_ticks(#[case] mode: ModeDefinition) {
        assert_eq!(run(&mode, 42, 100), run(&mode, 42, 100));
        assert_ne!(run(&mode, 42, 100), run(&mode, 43, 100));
    }

    // --- Properties ---

    /// Repeating steps as (interval ms, jitter ms, probability), often at the
    /// limits.
    fn schedules() -> impl Strategy<Value = Vec<(u64, u64, u8)>> {
        vec(
            (
                prop_oneof![Just(250_u64), 250..=5_000_u64],
                prop_oneof![Just(0_u64), 0..=5_000_u64],
                prop_oneof![Just(100_u8), 1..=100_u8],
            ),
            1..=9,
        )
    }

    /// One repeating step per schedule. Each selects its own hotbar slot, so
    /// a planned action tells which step ran.
    fn marked_mode(schedules: &[(u64, u64, u8)]) -> ModeDefinition {
        definition(
            schedules
                .iter()
                .zip(0_u8..)
                .map(|(&(interval, jitter, probability), n)| Step {
                    action: Action::SelectHotbarSlot { slot: slot(n) },
                    schedule: every(ms(interval), ms(jitter)),
                    probability,
                })
                .collect(),
        )
    }

    /// The steps of `marked_mode` that ran in `tick`.
    fn ran(tick: &PlanTick) -> Vec<usize> {
        tick.actions
            .iter()
            .map(|action| match action {
                PlannedAction::Game(GameAction::SelectHotbarSlot { slot }) => {
                    usize::from(slot.get())
                }
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    proptest! {
        #[test]
        fn on_time_runs_keep_their_gaps(
            schedules in schedules(),
            seed in any::<u64>(),
            ticks in 1..=200_usize,
        ) {
            let mut rng = rng(seed);
            let (mut plan, first) = ModePlan::start(&marked_mode(&schedules), start(), &mut rng);
            prop_assert_eq!(first.actions, Vec::new());

            // The start counts as the run before the first one.
            let mut last_run = vec![start(); schedules.len()];
            let mut due = first.next_due;
            for _ in 0..ticks {
                let now = due.unwrap();
                let tick = plan.tick(now, &mut rng);
                prop_assert!(tick.next_due.unwrap() > now);
                for index in ran(&tick) {
                    let (interval, _, _) = schedules[index];
                    let gap = time::elapsed(last_run[index], now);
                    prop_assert!(gap >= ms(interval), "step {index} ran after {gap:?}");
                    last_run[index] = now;
                }
                // Steps that always run are never late, so their gap is at most
                // interval + jitter.
                for (index, &(interval, jitter, probability)) in schedules.iter().enumerate() {
                    if probability == 100 {
                        prop_assert!(time::elapsed(last_run[index], now) <= ms(interval + jitter));
                    }
                }
                due = tick.next_due;
            }
        }

        #[test]
        fn late_ticks_never_catch_up(
            schedules in schedules(),
            seed in any::<u64>(),
            lateness in vec(prop_oneof![Just(0_u64), 1..=10_000_u64], 1..=100),
        ) {
            let mut rng = rng(seed);
            let (mut plan, first) = ModePlan::start(&marked_mode(&schedules), start(), &mut rng);

            let mut last_run = vec![start(); schedules.len()];
            let mut due = first.next_due;
            for late in lateness {
                let now = time::add(due.unwrap(), ms(late));
                let tick = plan.tick(now, &mut rng);
                prop_assert!(tick.next_due.unwrap() > now);

                let mut steps = ran(&tick);
                let count = steps.len();
                steps.dedup();
                prop_assert_eq!(steps.len(), count, "a step ran twice in one tick");
                for index in steps {
                    let gap = time::elapsed(last_run[index], now);
                    prop_assert!(gap >= ms(schedules[index].0), "step {index} ran after {gap:?}");
                    last_run[index] = now;
                }
                due = tick.next_due;
            }
        }

        #[test]
        fn a_tick_before_the_next_due_time_never_runs_anything(
            schedules in schedules(),
            seed in any::<u64>(),
            offsets in vec(-100_000_000_i64..100_000_000, 1..=50),
        ) {
            let mut rng = rng(seed);
            let (mut plan, first) = ModePlan::start(&marked_mode(&schedules), start(), &mut rng);
            let mut due = first.next_due.unwrap();
            for offset in offsets {
                let now = start() + chrono::TimeDelta::milliseconds(offset);
                let tick = plan.tick(now, &mut rng);
                if now < due {
                    prop_assert!(tick.actions.is_empty());
                    prop_assert_eq!(tick.next_due, Some(due));
                }
                due = tick.next_due.unwrap();
                prop_assert_eq!(plan.next_due(), Some(due));
            }
        }

        #[test]
        fn turns_stay_within_their_maxima(
            max_yaw in prop_oneof![Just(0.0_f32), Just(180.0_f32), 0.0_f32..=180.0],
            max_pitch in prop_oneof![Just(0.0_f32), Just(90.0_f32), 0.0_f32..=90.0],
            seed in any::<u64>(),
        ) {
            prop_assume!(max_yaw > 0.0 || max_pitch > 0.0);
            let (yaw, pitch) = turn(max_yaw, max_pitch, seed);
            prop_assert!(yaw.abs() <= max_yaw && pitch.abs() <= max_pitch);
        }
    }
}
