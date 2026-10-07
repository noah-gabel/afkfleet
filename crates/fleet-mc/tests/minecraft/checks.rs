//! Checks of a bot's state through RCON, for the scenarios that act: the
//! actions and the fault containment. Like them, it needs the
//! `fault-injection` feature.

use std::time::Instant;

use fleet_core::mc::SessionHandle;
use fleet_mc::McSession;

use crate::harness::{Server, WITHIN, within};

impl Server {
    /// Runs `command` until its output satisfies `check`, back to back
    /// without sleeping (each exec takes a while), and returns that output.
    /// Fails the test with `what` and the last output after [`WITHIN`].
    pub(crate) async fn eventually(
        &self,
        what: &str,
        command: &str,
        check: impl Fn(&str) -> bool,
    ) -> String {
        let deadline = Instant::now() + WITHIN;
        loop {
            let output = self.rcon(command).await;
            if check(&output) {
                return output;
            }
            assert!(
                Instant::now() < deadline,
                "{what}: `{command}` still answered {output:?} after {WITHIN:?}"
            );
        }
    }

    /// Waits until the server sees `name` looking at `yaw` and `pitch`,
    /// within half a degree: azalea rounds a look to the mouse's steps.
    pub(crate) async fn sees_rotation(&self, name: &str, yaw: f32, pitch: f32) {
        self.eventually(
            &format!("{name} looking at ({yaw}, {pitch})"),
            &format!("data get entity {name} Rotation"),
            |output| match data_floats(output).as_slice() {
                [actual_yaw, actual_pitch] => {
                    angle_distance(*actual_yaw, yaw) < 0.5 && (actual_pitch - pitch).abs() < 0.5
                }
                _ => false,
            },
        )
        .await;
    }
}

/// The numbers of a `data get` answer, such as `… data: [90.0f, 30.0f]` or
/// `… data: 3`.
pub(crate) fn data_floats(output: &str) -> Vec<f32> {
    let Some((_, data)) = output.rsplit_once(": ") else {
        return Vec::new();
    };
    data.trim_matches(['[', ']'])
        .split(',')
        .filter_map(|number| {
            number
                .trim()
                .trim_end_matches(['f', 'd', 'b', 's'])
                .parse()
                .ok()
        })
        .collect()
}

/// The score of a `scoreboard players get` answer, such as
/// `AfkBot1 has 3 [jumps]`.
pub(crate) fn score(output: &str) -> Option<i64> {
    output
        .split_once(" has ")?
        .1
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// How far apart two yaws are, in degrees, whichever way round.
fn angle_distance(a: f32, b: f32) -> f32 {
    let difference = (a - b).rem_euclid(360.0);
    difference.min(360.0 - difference)
}

/// Waits until `session` has run `ticks` more game ticks, without sleeping.
pub(crate) async fn ticks_pass(session: &McSession, ticks: usize) {
    within("game ticks", async {
        for _ in 0..ticks {
            let before = session.liveness().last_tick;
            while session.liveness().last_tick <= before {
                tokio::task::yield_now().await;
            }
        }
    })
    .await;
}
