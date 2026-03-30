use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::meta::error::MetaResult;
use crate::meta::track::TrackInfo;

const MERGE_WINDOW_MS: u64 = 2_000;

#[derive(Debug)]
pub struct TimelineStore {
    path: PathBuf,
    inner: Mutex<BTreeMap<u64, TrackInfo>>,
}

impl TimelineStore {
    pub fn new(path: PathBuf) -> Self {
        let inner = load_tracks(&path).unwrap_or_default();
        Self {
            path,
            inner: Mutex::new(inner),
        }
    }

    pub fn clear(&self) -> MetaResult<()> {
        let mut inner = self.inner.lock().expect("timeline mutex poisoned");
        inner.clear();
        drop(inner);

        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    pub fn insert_tracks<I>(&self, tracks: I) -> MetaResult<bool>
    where
        I: IntoIterator<Item = TrackInfo>,
    {
        let mut inner = self.inner.lock().expect("timeline mutex poisoned");
        let mut changed = false;

        for track in tracks {
            if let Some(existing_key) = find_matching_track_key(&inner, &track) {
                if existing_key != track.start_time_ms {
                    inner.remove(&existing_key);
                    changed = true;
                }
            }

            let replace = inner
                .get(&track.start_time_ms)
                .map(|existing| existing != &track)
                .unwrap_or(true);
            if replace {
                inner.insert(track.start_time_ms, track);
                changed = true;
            }
        }

        if changed {
            persist_tracks(&self.path, &inner)?;
        }

        Ok(changed)
    }

    pub fn prune_before(&self, min_timestamp_ms: u64) -> MetaResult<bool> {
        let mut inner = self.inner.lock().expect("timeline mutex poisoned");
        let before = inner.len();
        inner.retain(|_, track| track.end_time_ms() >= min_timestamp_ms);
        let changed = inner.len() != before;

        if changed {
            persist_tracks(&self.path, &inner)?;
        }

        Ok(changed)
    }

    pub fn track_for_cursor(&self, cursor_ms: u64) -> Option<TrackInfo> {
        let inner = self.inner.lock().expect("timeline mutex poisoned");

        if let Some(track) = inner
            .range(..=cursor_ms)
            .next_back()
            .map(|(_, track)| track)
            .filter(|track| track.contains_timestamp_ms(cursor_ms))
        {
            return Some(track.clone());
        }

        inner
            .range(..=cursor_ms)
            .next_back()
            .map(|(_, track)| track.clone())
    }

    pub fn latest_track(&self) -> Option<TrackInfo> {
        let inner = self.inner.lock().expect("timeline mutex poisoned");
        inner.iter().next_back().map(|(_, track)| track.clone())
    }
    #[cfg(test)]
    pub fn next_after(&self, cursor_ms: u64) -> Option<TrackInfo> {
        let inner = self.inner.lock().expect("timeline mutex poisoned");
        inner
            .range((cursor_ms.saturating_add(1))..)
            .next()
            .map(|(_, track)| track.clone())
    }
}

fn load_tracks(path: &Path) -> Option<BTreeMap<u64, TrackInfo>> {
    let raw = fs::read_to_string(path).ok()?;
    let tracks: Vec<TrackInfo> = serde_json::from_str(&raw).ok()?;
    Some(
        tracks
            .into_iter()
            .map(|track| (track.start_time_ms, track))
            .collect(),
    )
}

fn persist_tracks(path: &Path, tracks: &BTreeMap<u64, TrackInfo>) -> MetaResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let serialized = serde_json::to_string_pretty(&tracks.values().cloned().collect::<Vec<_>>())?;
    fs::write(path, serialized)?;
    Ok(())
}

fn find_matching_track_key(tracks: &BTreeMap<u64, TrackInfo>, track: &TrackInfo) -> Option<u64> {
    let window_start = track.start_time_ms.saturating_sub(MERGE_WINDOW_MS);
    let window_end = track.start_time_ms.saturating_add(MERGE_WINDOW_MS);

    tracks
        .range(window_start..=window_end)
        .find(|(_, existing)| same_track_identity(existing, track))
        .map(|(start_time_ms, _)| *start_time_ms)
}

fn same_track_identity(left: &TrackInfo, right: &TrackInfo) -> bool {
    left.artist == right.artist
        && left.title == right.title
        && left.duration_secs == right.duration_secs
        && (left.album == right.album || left.album.is_empty() || right.album.is_empty())
}

#[cfg(test)]
mod tests {
    use super::TimelineStore;
    use crate::meta::track::TrackInfo;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("listenmoe-{name}-{unique}.json"))
    }

    fn track(start_time_ms: u64, duration_secs: u32, album: &str) -> TrackInfo {
        TrackInfo {
            artist: "artist".into(),
            title: format!("title-{start_time_ms}"),
            album: album.into(),
            album_cover: None,
            artist_image: None,
            start_time_ms,
            duration_secs,
        }
    }

    fn titled_track(start_time_ms: u64, duration_secs: u32, title: &str, album: &str) -> TrackInfo {
        TrackInfo {
            artist: "artist".into(),
            title: title.into(),
            album: album.into(),
            album_cover: None,
            artist_image: None,
            start_time_ms,
            duration_secs,
        }
    }

    #[test]
    fn dedupes_by_start_time_and_replaces_richer_metadata() {
        let path = temp_file("timeline-dedupe");
        let store = TimelineStore::new(path.clone());
        store
            .insert_tracks([track(10_000, 10, "")])
            .expect("insert failed");
        store
            .insert_tracks([track(10_000, 10, "album")])
            .expect("replace failed");

        let current = store.track_for_cursor(10_500).expect("missing track");
        assert_eq!(current.album, "album");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn merges_same_track_with_nearby_start_times() {
        let path = temp_file("timeline-nearby-dedupe");
        let store = TimelineStore::new(path.clone());

        store
            .insert_tracks([titled_track(10_000, 10, "same-song", "album")])
            .expect("insert failed");
        store
            .insert_tracks([titled_track(10_850, 10, "same-song", "album")])
            .expect("replace failed");

        assert!(store.track_for_cursor(10_200).is_none());
        assert_eq!(
            store
                .track_for_cursor(11_000)
                .expect("missing merged track")
                .start_time_ms,
            10_850
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn finds_current_and_next_track_for_cursor() {
        let path = temp_file("timeline-cursor");
        let store = TimelineStore::new(path.clone());
        store
            .insert_tracks([track(1_000, 10, "one"), track(12_000, 10, "two")])
            .expect("insert failed");

        assert_eq!(
            store
                .track_for_cursor(5_000)
                .expect("missing current")
                .album,
            "one"
        );
        assert_eq!(store.next_after(5_000).expect("missing next").album, "two");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn returns_latest_track() {
        let path = temp_file("timeline-latest");
        let store = TimelineStore::new(path.clone());
        store
            .insert_tracks([track(1_000, 10, "one"), track(12_000, 10, "two")])
            .expect("insert failed");

        assert_eq!(store.latest_track().expect("missing latest").album, "two");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn prunes_tracks_before_timestamp() {
        let path = temp_file("timeline-prune");
        let store = TimelineStore::new(path.clone());
        store
            .insert_tracks([track(1_000, 1, "old"), track(10_000, 10, "new")])
            .expect("insert failed");

        store.prune_before(5_000).expect("prune failed");

        assert!(store.track_for_cursor(1_200).is_none());
        assert_eq!(
            store
                .track_for_cursor(12_000)
                .expect("missing retained")
                .album,
            "new"
        );

        let _ = std::fs::remove_file(path);
    }
}
