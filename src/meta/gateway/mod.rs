use std::sync::mpsc;
use std::sync::{atomic::AtomicU64, Arc};
use std::thread;
use std::time::Duration;

use crate::meta::controller::Control;
use crate::meta::error::MetaResult;
use crate::meta::track::TrackInfo;
use crate::station::Station;

mod control;
mod model;
mod parse;
mod session;

use control::{handle_outer_control, OuterLoopAction};

/// Outer reconnect loop using blocking tungstenite.
pub fn run_meta_loop(
    station: Station,
    sender: mpsc::Sender<TrackInfo>,
    rx: mpsc::Receiver<Control>,
    lag_ms: Arc<AtomicU64>,
    ui_sched_id: Arc<AtomicU64>,
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
            lag_ms.clone(),
            ui_sched_id.clone(),
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
