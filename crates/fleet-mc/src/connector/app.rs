//! A session's Bevy App, as ADR-0008 §1 and §3 decided: azalea's plugins
//! without auto-reconnect and auto-respawn, the packet-liveness plugin, and
//! the single-threaded executor on every schedule.

use std::sync::Arc;

use azalea::DefaultPlugins;
use azalea::app::{App, PluginGroup as _};
use azalea::auto_reconnect::AutoReconnectPlugin;
use azalea::auto_respawn::AutoRespawnPlugin;
use azalea::bot::DefaultBotPlugins;
use azalea::ecs::schedule::{ExecutorKind, Schedules};

use crate::events::{LivenessStamps, PacketLivenessPlugin};

/// Builds the App of one session. `extra` adds to it before the executor is
/// set, so whatever it adds runs single-threaded too (the fault-injection
/// hook's systems, in the slow tests).
pub(super) fn build_app(stamps: Arc<LivenessStamps>, extra: impl FnOnce(&mut App)) -> App {
    let mut app = App::new();
    app.add_plugins((
        DefaultPlugins,
        // The bot's lifecycle is the core state machine's (ADR-0008 §1).
        DefaultBotPlugins
            .build()
            .disable::<AutoReconnectPlugin>()
            .disable::<AutoRespawnPlugin>(),
        PacketLivenessPlugin(stamps),
    ));
    extra(&mut app);
    single_threaded(&mut app);
    app
}

/// Runs every schedule on the App's own thread (ADR-0008 §3): with Bevy's
/// multi-threaded executor, every App shares a process-wide pool, so one hung
/// session would freeze them all.
fn single_threaded(app: &mut App) {
    let mut schedules = app.world_mut().resource_mut::<Schedules>();
    for (_, schedule) in schedules.iter_mut() {
        schedule.set_executor_kind(ExecutorKind::SingleThreaded);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn stamps() -> Arc<LivenessStamps> {
        Arc::new(LivenessStamps::new(Instant::now()))
    }

    /// Every schedule's executor kind.
    fn executors(app: &App) -> Vec<ExecutorKind> {
        app.world()
            .resource::<Schedules>()
            .iter()
            .map(|(_, schedule)| schedule.get_executor_kind())
            .collect()
    }

    #[test]
    fn every_schedule_runs_single_threaded() {
        let app = build_app(stamps(), |_| {});

        let executors = executors(&app);

        assert_ne!(executors.len(), 0);
        assert!(
            executors
                .iter()
                .all(|kind| *kind == ExecutorKind::SingleThreaded),
            "{executors:?}"
        );
    }

    #[test]
    fn what_extra_adds_runs_single_threaded_too() {
        let app = build_app(stamps(), |app| {
            for (_, schedule) in app.world_mut().resource_mut::<Schedules>().iter_mut() {
                schedule.set_executor_kind(ExecutorKind::MultiThreaded);
            }
        });

        assert!(
            executors(&app)
                .iter()
                .all(|kind| *kind == ExecutorKind::SingleThreaded)
        );
    }

    #[test]
    fn auto_reconnect_and_auto_respawn_are_left_out() {
        let app = build_app(stamps(), |_| {});

        assert!(!app.is_plugin_added::<AutoReconnectPlugin>());
        assert!(!app.is_plugin_added::<AutoRespawnPlugin>());
    }

    #[test]
    fn azaleas_bot_plugins_and_the_liveness_plugin_are_added() {
        let app = build_app(stamps(), |_| {});

        assert!(app.is_plugin_added::<azalea::bot::BotPlugin>());
        assert!(app.is_plugin_added::<PacketLivenessPlugin>());
    }
}
