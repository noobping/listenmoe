use std::sync::mpsc;
use std::sync::{atomic::AtomicU64, Arc};
use std::thread;
use std::time::Duration;

use crate::listen::PlaybackClock;
use crate::meta::controller::Control;
use crate::meta::error::MetaResult;
use crate::meta::timeline::TimelineStore;
use crate::station::Station;
use crate::ui::UiEvent;

mod control;
mod model;
mod parse;
mod session;

use control::{handle_outer_control, OuterLoopAction};

/// Outer reconnect loop using blocking tungstenite.
pub fn run_meta_loop(
    station: Station,
    sender: mpsc::Sender<UiEvent>,
    rx: mpsc::Receiver<Control>,
    clock: Arc<PlaybackClock>,
    ui_sched_id: Arc<AtomicU64>,
    timeline: Arc<TimelineStore>,
) -> MetaResult<()> {
    let mut paused = false;
    let retry_delay = Duration::from_secs(5);

    loop {
        match handle_outer_control(&rx, &mut paused, &ui_sched_id, Duration::ZERO) {
            OuterLoopAction::Stop => return Ok(()),
            OuterLoopAction::Sleep(wait) => thread::sleep(wait),
            OuterLoopAction::Continue => {}
        }

        match session::run_once(
            station,
            sender.clone(),
            &rx,
            clock.clone(),
            ui_sched_id.clone(),
            timeline.clone(),
            &mut paused,
        ) {
            Ok(()) => {}
            Err(err) => {
                eprintln!("Gateway connection error: {err}, retrying in 5s…");
            }
        }

        // Session ended or failed: apply control/backoff policy once.
        match handle_outer_control(&rx, &mut paused, &ui_sched_id, retry_delay) {
            OuterLoopAction::Stop => return Ok(()),
            OuterLoopAction::Sleep(wait) => thread::sleep(wait),
            OuterLoopAction::Continue => {}
        }
    }
}
