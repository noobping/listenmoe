use std::{cell::RefCell, rc::Rc};

pub(super) type CoverFetchResult = (String, Result<Vec<u8>, String>);
pub(super) type SharedTrack = Rc<RefCell<Option<(String, String)>>>;
pub(super) type MetadataSetter = Rc<dyn Fn(&str, &str, Option<&str>)>;

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

    pub(super) fn set_track(&self, artist: &str, title: &str) {
        *self.current_track.borrow_mut() = Some((artist.to_string(), title.to_string()));
    }

    pub(super) fn set_latest_cover_url(&mut self, url: Option<&str>) {
        self.latest_cover_url = url.map(str::to_owned);
    }

    pub(super) fn is_latest_cover(&self, url: &str) -> bool {
        self.latest_cover_url.as_deref() == Some(url)
    }
}
