use serde_json::Value;

use crate::meta::time_parse::parse_rfc3339_system_time;
use crate::meta::track::TrackInfo;

use super::model::GatewaySongPayload;

/// Extract artist(s) + title from the gateway payload.
pub(super) fn parse_track_info(d: &Value) -> Option<TrackInfo> {
    let payload: GatewaySongPayload = serde_json::from_value(d.clone()).ok()?;

    let start_time_utc = parse_rfc3339_system_time(&payload.start_time)?;
    let track = payload.song;

    Some(TrackInfo {
        artist: track.display_artist(),
        title: track.display_title(),
        album_cover: track.album_cover_url(),
        artist_image: track.artist_image_url(),
        start_time_utc,
        duration_secs: track.duration_secs(),
    })
}
