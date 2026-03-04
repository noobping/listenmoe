use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, SystemTime};

use super::track::TrackInfo;

pub fn pick_track_for_playback(history: &VecDeque<TrackInfo>, lag_ms: u64) -> Option<TrackInfo> {
    let playback_now = SystemTime::now().checked_sub(Duration::from_millis(lag_ms))?;

    // Prefer a proper [start, end) window when duration is known and > 0.
    if let Some(hit) = history.iter().rev().find(|t| {
        if t.duration_secs == 0 {
            return false;
        }
        let start = t.start_time_utc;
        let end = start.checked_add(Duration::from_secs(t.duration_secs as u64));
        end.map(|end| playback_now >= start && playback_now < end)
            .unwrap_or(false)
    }) {
        return Some(hit.clone());
    }

    // Fallback: duration is missing/0 => pick the latest track that started before playback_now.
    history
        .iter()
        .rev()
        .find(|t| playback_now >= t.start_time_utc)
        .cloned()
}

pub fn schedule_ui_switch(
    sender: mpsc::Sender<TrackInfo>,
    track: TrackInfo,
    lag_ms: u64,
    ui_sched_id: Arc<AtomicU64>,
    my_id: u64,
) {
    thread::spawn(move || {
        let lag = Duration::from_millis(lag_ms);
        let target = track.start_time_utc.checked_add(lag);
        if let Some(target) = target {
            if let Ok(wait) = target.duration_since(SystemTime::now()) {
                thread::sleep(wait);
            }
        }
        if ui_sched_id.load(Ordering::Relaxed) == my_id {
            let _ = sender.send(track);
        }
    });
}

pub fn schedule_next_from_history(
    sender: mpsc::Sender<TrackInfo>,
    history: &VecDeque<TrackInfo>,
    lag_ms: u64,
    ui_sched_id: Arc<AtomicU64>,
) {
    let playback_now = match SystemTime::now().checked_sub(Duration::from_millis(lag_ms)) {
        Some(t) => t,
        None => return,
    };

    // Find the earliest track whose (start_time_utc) is still in the future for playback time.
    // i.e. playback_now < track.start_time_utc
    let next = history
        .iter()
        .filter(|t| playback_now < t.start_time_utc)
        .min_by_key(|t| t.start_time_utc)
        .cloned();

    let Some(next) = next else { return };

    let my_id = ui_sched_id.fetch_add(1, Ordering::Relaxed) + 1;

    #[cfg(debug_assertions)]
    println!(
        "[{}] ui {} resched-next: {} - {} (lag_ms={})",
        crate::log::now_string(),
        my_id,
        next.artist,
        next.title,
        lag_ms
    );

    schedule_ui_switch(sender, next, lag_ms, ui_sched_id, my_id);
}
