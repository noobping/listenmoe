use crate::meta::TrackInfo;

use std::{cell::RefCell, rc::Rc};

use super::super::controls::NowPlaying;

pub(super) type CoverFetchResult = (String, Result<Vec<u8>, String>);
pub(super) type SharedTrack = Rc<RefCell<Option<(String, String)>>>;
pub(super) type MetadataSetter = Rc<dyn Fn(Option<NowPlaying>)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiResetReason {
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
