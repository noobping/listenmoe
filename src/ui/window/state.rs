use crate::meta::TrackInfo;

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use super::super::controls::NowPlaying;

#[cfg(target_os = "linux")]
use crate::volume::VolumeEvent;

pub(super) type CoverFetchResult = (String, Result<Vec<u8>, String>);
pub(super) type SharedTrack = Rc<RefCell<Option<(String, String)>>>;
pub(super) type SharedTitle = Rc<RefCell<(String, String)>>;
pub(super) type SharedFlag = Rc<Cell<bool>>;
pub(super) type MetadataSetter = Rc<dyn Fn(Option<NowPlaying>)>;

#[cfg(target_os = "linux")]
pub(super) type SharedVolumeState = Rc<RefCell<VolumeState>>;

/// Keeps desktop-stream volume and the software fallback from being applied at
/// the same time. The displayed percentage is retained when a stream goes
/// away, so the control continues to work before playback and during backend
/// reconnects.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VolumeState {
    percent: u8,
    desktop_available: bool,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VolumeUpdate {
    pub(super) display_percent: u8,
    pub(super) software_percent: u8,
}

#[cfg(target_os = "linux")]
impl VolumeState {
    pub(super) fn new(initial_percent: u8) -> Self {
        Self {
            percent: initial_percent.min(100),
            desktop_available: false,
        }
    }

    pub(super) fn apply_local_request(&mut self, percent: u8) -> VolumeUpdate {
        self.percent = percent.min(100);
        self.current_update()
    }

    pub(super) fn apply_backend_event(&mut self, event: VolumeEvent) -> VolumeUpdate {
        match event {
            // Do not enable software gain merely because the controller lost
            // its connection: the PipeWire playback node and its gain may
            // still be active, which would otherwise double-attenuate audio.
            VolumeEvent::Disconnected => {}
            VolumeEvent::Unavailable => {
                self.desktop_available = false;
            }
            VolumeEvent::Available { raw_percent, muted } => {
                self.desktop_available = true;
                self.percent = if muted { 0 } else { raw_percent.min(100) as u8 };
            }
        }

        self.current_update()
    }

    fn current_update(self) -> VolumeUpdate {
        VolumeUpdate {
            display_percent: self.percent,
            // Desktop stream gain and app gain must never compound.
            software_percent: if self.desktop_available {
                100
            } else {
                self.percent
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiResetReason {
    #[cfg(feature = "experimental")]
    Paused,
    Stopped,
}

#[derive(Debug, Clone)]
pub enum UiEvent {
    Connecting,
    Reset(UiResetReason),
    TrackChanged(TrackInfo),
}

pub(super) struct RuntimeState {
    current_track: SharedTrack,
    latest_cover_url: Option<String>,
}

impl RuntimeState {
    pub(super) fn new(current_track: SharedTrack) -> Self {
        Self {
            current_track,
            latest_cover_url: None,
        }
    }

    pub(super) fn set_track(&self, track: &TrackInfo) {
        *self.current_track.borrow_mut() = Some((track.artist.clone(), track.title.clone()));
    }

    pub(super) fn clear_track(&self) {
        *self.current_track.borrow_mut() = None;
    }

    pub(super) fn set_latest_cover_url(&mut self, url: Option<&str>) {
        self.latest_cover_url = url.map(str::to_owned);
    }

    pub(super) fn is_latest_cover(&self, url: &str) -> bool {
        self.latest_cover_url.as_deref() == Some(url)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod volume_tests {
    use super::{VolumeState, VolumeUpdate};
    use crate::volume::VolumeEvent;

    #[test]
    fn unavailable_backend_uses_software_fallback() {
        let mut state = VolumeState::new(73);

        assert_eq!(
            state.apply_backend_event(VolumeEvent::Unavailable),
            VolumeUpdate {
                display_percent: 73,
                software_percent: 73,
            }
        );
    }

    #[test]
    fn controller_disconnect_keeps_the_previous_gain_mode() {
        let mut state = VolumeState::new(73);

        assert_eq!(
            state.apply_backend_event(VolumeEvent::Disconnected),
            VolumeUpdate {
                display_percent: 73,
                software_percent: 73,
            }
        );

        state.apply_backend_event(VolumeEvent::Available {
            raw_percent: 42,
            muted: false,
        });
        assert_eq!(
            state.apply_backend_event(VolumeEvent::Disconnected),
            VolumeUpdate {
                display_percent: 42,
                software_percent: 100,
            }
        );
    }

    #[test]
    fn available_backend_is_canonical_without_double_attenuation() {
        let mut state = VolumeState::new(73);

        assert_eq!(
            state.apply_backend_event(VolumeEvent::Available {
                raw_percent: 42,
                muted: false,
            }),
            VolumeUpdate {
                display_percent: 42,
                software_percent: 100,
            }
        );
    }

    #[test]
    fn local_request_is_retained_for_a_later_software_fallback() {
        let mut state = VolumeState::new(100);
        state.apply_backend_event(VolumeEvent::Available {
            raw_percent: 61,
            muted: false,
        });

        assert_eq!(
            state.apply_local_request(35),
            VolumeUpdate {
                display_percent: 35,
                software_percent: 100,
            }
        );
        assert_eq!(
            state.apply_backend_event(VolumeEvent::Unavailable),
            VolumeUpdate {
                display_percent: 35,
                software_percent: 35,
            }
        );
    }

    #[test]
    fn muted_and_amplified_backend_values_are_safe_for_the_ui() {
        let mut state = VolumeState::new(100);

        assert_eq!(
            state.apply_backend_event(VolumeEvent::Available {
                raw_percent: 150,
                muted: false,
            }),
            VolumeUpdate {
                display_percent: 100,
                software_percent: 100,
            }
        );
        assert_eq!(
            state.apply_backend_event(VolumeEvent::Available {
                raw_percent: 68,
                muted: true,
            }),
            VolumeUpdate {
                display_percent: 0,
                software_percent: 100,
            }
        );
    }
}
