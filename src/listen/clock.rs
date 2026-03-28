use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct PlaybackClock {
    live_head_ms: AtomicU64,
    playback_cursor_ms: AtomicU64,
}

impl PlaybackClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&self) {
        self.live_head_ms.store(0, Ordering::Relaxed);
        self.playback_cursor_ms.store(0, Ordering::Relaxed);
    }

    pub fn live_head_ms(&self) -> u64 {
        self.live_head_ms.load(Ordering::Relaxed)
    }

    pub fn playback_cursor_ms(&self) -> u64 {
        self.playback_cursor_ms.load(Ordering::Relaxed)
    }

    pub fn set_live_head_ms(&self, value: u64) {
        self.live_head_ms.store(value, Ordering::Relaxed);
    }

    pub fn set_playback_cursor_ms(&self, value: u64) {
        self.playback_cursor_ms.store(value, Ordering::Relaxed);
    }

    pub fn snap_playback_to_live(&self) -> u64 {
        let live = self.live_head_ms();
        self.set_playback_cursor_ms(live);
        live
    }

    pub fn clamp_playback_floor(&self, floor_ms: u64) -> u64 {
        let current = self.playback_cursor_ms();
        let clamped = current.max(floor_ms);
        if clamped != current {
            self.set_playback_cursor_ms(clamped);
        }
        clamped
    }
}
