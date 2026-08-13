use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

const MAX_PERCENT: u8 = 100;

/// Volume shared between the UI and the active playback stream.
#[derive(Debug, Clone)]
pub(in crate::listen) struct PlaybackVolume(Arc<AtomicU8>);

impl PlaybackVolume {
    pub(in crate::listen) fn percent(&self) -> u8 {
        self.0.load(Ordering::Relaxed).min(MAX_PERCENT)
    }

    pub(in crate::listen) fn set_percent(&self, percent: u8) {
        self.0.store(percent.min(MAX_PERCENT), Ordering::Relaxed);
    }

    pub(in crate::listen) fn factor(&self) -> f32 {
        f32::from(self.percent()) / f32::from(MAX_PERCENT)
    }
}

impl Default for PlaybackVolume {
    fn default() -> Self {
        Self(Arc::new(AtomicU8::new(MAX_PERCENT)))
    }
}

#[cfg(test)]
mod tests {
    use super::PlaybackVolume;

    #[test]
    fn defaults_to_full_volume() {
        let volume = PlaybackVolume::default();

        assert_eq!(volume.percent(), 100);
        assert_eq!(volume.factor(), 1.0);
    }

    #[test]
    fn clones_share_volume_changes() {
        let volume = PlaybackVolume::default();
        let playback = volume.clone();

        volume.set_percent(37);

        assert_eq!(playback.percent(), 37);
        assert_eq!(playback.factor(), 0.37);
    }

    #[test]
    fn volume_is_clamped_to_one_hundred_percent() {
        let volume = PlaybackVolume::default();

        volume.set_percent(u8::MAX);

        assert_eq!(volume.percent(), 100);
        assert_eq!(volume.factor(), 1.0);
    }

    #[test]
    fn zero_percent_is_silent() {
        let volume = PlaybackVolume::default();

        volume.set_percent(0);

        assert_eq!(volume.percent(), 0);
        assert_eq!(volume.factor(), 0.0);
    }
}
