use std::sync::mpsc;
use std::time::Duration;

use crate::meta::controller::Control;

pub(super) enum OuterLoopAction {
    Continue,
    Stop,
    Sleep(Duration),
}

pub(super) fn handle_outer_control(
    rx: &mpsc::Receiver<Control>,
    paused: &mut bool,
    empty_sleep: Duration,
) -> OuterLoopAction {
    #[cfg(not(feature = "experimental"))]
    let _ = paused;

    match rx.try_recv() {
        Ok(Control::Stop) | Err(mpsc::TryRecvError::Disconnected) => OuterLoopAction::Stop,
        #[cfg(feature = "experimental")]
        Ok(Control::Pause) => {
            *paused = true;
            OuterLoopAction::Sleep(Duration::from_secs(1))
        }
        #[cfg(feature = "experimental")]
        Ok(Control::Resume) => {
            *paused = false;
            OuterLoopAction::Sleep(Duration::from_secs(1))
        }
        Err(mpsc::TryRecvError::Empty) if empty_sleep.is_zero() => OuterLoopAction::Continue,
        Err(mpsc::TryRecvError::Empty) => OuterLoopAction::Sleep(empty_sleep),
    }
}
