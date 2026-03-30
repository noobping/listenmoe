use serde_json::Value;

use crate::meta::time_parse::parse_rfc3339_timestamp_ms;
use crate::meta::track::TrackInfo;

use super::model::{GatewaySongPayload, Song};

pub(super) struct ParsedTrackBatch {
    pub(super) current: TrackInfo,
    pub(super) history: Vec<TrackInfo>,
}

pub(super) fn parse_track_batch(d: &Value) -> Option<ParsedTrackBatch> {
    let payload: GatewaySongPayload = serde_json::from_value(d.clone()).ok()?;
    let current_start_ms = parse_rfc3339_timestamp_ms(&payload.start_time)?;

    let current = build_track_info(&payload.song, current_start_ms);
    let mut history = Vec::with_capacity(payload.last_played.len());
    let mut next_end_ms = current_start_ms;

    for song in &payload.last_played {
        let duration_ms = u64::from(song.duration_secs()).saturating_mul(1000);
        if duration_ms == 0 {
            break;
        }

        let start_time_ms = next_end_ms.saturating_sub(duration_ms);
        history.push(build_track_info(song, start_time_ms));
        next_end_ms = start_time_ms;
    }

    Some(ParsedTrackBatch { current, history })
}

pub(super) fn build_track_info(song: &Song, start_time_ms: u64) -> TrackInfo {
    TrackInfo {
        artist: song.display_artist(),
        title: song.display_title(),
        album: song.display_album(),
        album_cover: song.album_cover_url(),
        artist_image: song.artist_image_url(),
        start_time_ms,
        duration_secs: song.duration_secs(),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_track_batch;
    use serde_json::json;

    #[test]
    fn parses_current_track_album_and_backfills_last_played() {
        let payload = json!({
            "song": {
                "title": "Current",
                "artists": [{ "name": "Artist", "image": "artist.jpg" }],
                "albums": [{ "name": "Album", "image": "album.jpg" }],
                "duration": 180
            },
            "startTime": "2025-01-01T00:03:00.000Z",
            "lastPlayed": [
                {
                    "title": "Previous",
                    "artists": [{ "name": "Prev Artist", "image": null }],
                    "albums": [{ "name": "Prev Album", "image": null }],
                    "duration": 120
                }
            ]
        });

        let parsed = parse_track_batch(&payload).expect("payload should parse");
        assert_eq!(parsed.current.album, "Album");
        assert_eq!(parsed.current.artist, "Artist");
        assert_eq!(parsed.current.title, "Current");
        assert_eq!(parsed.history.len(), 1);
        assert_eq!(parsed.history[0].album, "Prev Album");
        assert_eq!(
            parsed.history[0].start_time_ms,
            parsed.current.start_time_ms - 120_000
        );
    }
}
