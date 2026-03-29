use std::sync::mpsc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
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
use session::sync_ui_track;

const UI_FOLLOW_INTERVAL: Duration = Duration::from_millis(200);

/// Outer reconnect loop using blocking tungstenite.
pub fn run_meta_loop(
    station: Station,
    sender: mpsc::Sender<UiEvent>,
    rx: mpsc::Receiver<Control>,
    clock: Arc<PlaybackClock>,
    timeline: Arc<TimelineStore>,
) -> MetaResult<()> {
    let mut paused = false;
    let retry_delay = Duration::from_secs(5);
    let paused_flag = Arc::new(AtomicBool::new(false));
    let stop_requested = Arc::new(AtomicBool::new(false));
    let ui_follow_handle = {
        let sender = sender.clone();
        let clock = clock.clone();
        let timeline = timeline.clone();
        let paused_flag = paused_flag.clone();
        let stop_requested = stop_requested.clone();
        thread::spawn(move || {
            let mut last_ui_track = None;

            while !stop_requested.load(Ordering::Relaxed) {
                if paused_flag.load(Ordering::Relaxed) {
                    last_ui_track = None;
                } else {
                    sync_ui_track(&sender, &timeline, &clock, &mut last_ui_track);
                }

                thread::sleep(UI_FOLLOW_INTERVAL);
            }
        })
    };

    let result = loop {
        match handle_outer_control(&rx, &mut paused, Duration::ZERO) {
            OuterLoopAction::Stop => break Ok(()),
            OuterLoopAction::Sleep(wait) => thread::sleep(wait),
            OuterLoopAction::Continue => {}
        }
        paused_flag.store(paused, Ordering::Relaxed);

        match session::run_once(
            station,
            &rx,
            paused_flag.clone(),
            clock.clone(),
            timeline.clone(),
            &mut paused,
        ) {
            Ok(()) => {}
            Err(err) => {
                eprintln!("Gateway connection error: {err}, retrying in 5s…");
            }
        }
        paused_flag.store(paused, Ordering::Relaxed);

        // Session ended or failed: apply control/backoff policy once.
        match handle_outer_control(&rx, &mut paused, retry_delay) {
            OuterLoopAction::Stop => break Ok(()),
            OuterLoopAction::Sleep(wait) => thread::sleep(wait),
            OuterLoopAction::Continue => {}
        }
        paused_flag.store(paused, Ordering::Relaxed);
    };

    stop_requested.store(true, Ordering::Relaxed);
    let _ = ui_follow_handle.join();
    result
}
