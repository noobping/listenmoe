use std::sync::mpsc;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

use crate::meta::controller::Control;

pub(super) enum OuterLoopAction {
    Continue,
    Stop,
    Sleep(Duration),
}

pub(super) fn invalidate_ui_schedule(ui_sched_id: &Arc<AtomicU64>) {
    ui_sched_id.fetch_add(1, Ordering::Relaxed);
}

pub(super) fn handle_outer_control(
    rx: &mpsc::Receiver<Control>,
    paused: &mut bool,
    ui_sched_id: &Arc<AtomicU64>,
    empty_sleep: Duration,
) -> OuterLoopAction {
    match rx.try_recv() {
        Ok(Control::Stop) | Err(mpsc::TryRecvError::Disconnected) => OuterLoopAction::Stop,
        Ok(Control::Pause) => {
            *paused = true;
            invalidate_ui_schedule(ui_sched_id);
            OuterLoopAction::Sleep(Duration::from_secs(1))
        }
        Ok(Control::Resume) => {
            *paused = false;
            invalidate_ui_schedule(ui_sched_id);
            OuterLoopAction::Sleep(Duration::from_secs(1))
        }
        Err(mpsc::TryRecvError::Empty) if empty_sleep.is_zero() => OuterLoopAction::Continue,
        Err(mpsc::TryRecvError::Empty) => OuterLoopAction::Sleep(empty_sleep),
    }
}
