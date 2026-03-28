use serde::{Deserialize, Serialize};

pub const ALBUM_COVER_BASE: &str = "https://cdn.listen.moe/covers/";
pub const ARTIST_IMAGE_BASE: &str = "https://cdn.listen.moe/artists/";

/// Track info sent to the UI thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackInfo {
    pub artist: String,
    pub title: String,
    pub album: String,
    pub album_cover: Option<String>,
    pub artist_image: Option<String>,
    pub start_time_ms: u64,
    pub duration_secs: u32,
}

impl TrackInfo {
    pub fn end_time_ms(&self) -> u64 {
        self.start_time_ms
            .saturating_add(u64::from(self.duration_secs).saturating_mul(1000))
    }

    pub fn contains_timestamp_ms(&self, timestamp_ms: u64) -> bool {
        if self.duration_secs == 0 {
            return timestamp_ms >= self.start_time_ms;
        }

        timestamp_ms >= self.start_time_ms && timestamp_ms < self.end_time_ms()
    }
}
