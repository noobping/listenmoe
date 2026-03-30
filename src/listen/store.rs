#[cfg(test)]
use std::collections::VecDeque;
use std::fs;
#[cfg(test)]
use std::fs::{File, OpenOptions};
#[cfg(test)]
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use super::Result;

pub const RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const GAP_THRESHOLD_MS: u64 = 1_500;
#[cfg(test)]
const MAX_SEGMENT_BYTES: u64 = 512 * 1024 * 1024;
#[cfg(test)]
const BYTES_PER_SAMPLE: u64 = 4;

pub fn compute_chunk_timing(
    previous_live_head_ms: u64,
    sample_rate: u32,
    channels: u16,
    sample_count: usize,
    now_ms: u64,
) -> Option<(u64, u64)> {
    if sample_count == 0 || sample_rate == 0 || channels == 0 {
        return None;
    }

    let frames = (sample_count / usize::from(channels)) as u64;
    if frames == 0 {
        return None;
    }

    let duration_ms = ((frames.saturating_mul(1000)) / u64::from(sample_rate)).max(1);
    let expected_start_ms = if previous_live_head_ms == 0 {
        now_ms.saturating_sub(duration_ms)
    } else {
        previous_live_head_ms
    };
    let wall_start_ms = now_ms.saturating_sub(duration_ms);
    let start_ms = if previous_live_head_ms != 0
        && wall_start_ms > expected_start_ms.saturating_add(GAP_THRESHOLD_MS)
    {
        wall_start_ms
    } else {
        expected_start_ms
    };
    let end_ms = start_ms.saturating_add(duration_ms);

    Some((start_ms, end_ms))
}

#[derive(Debug, Clone)]
pub struct StoredPcmChunk {
    pub channels: u16,
    pub sample_rate: u32,
    pub samples: Vec<f32>,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone)]
#[cfg(test)]
pub struct PlaybackReadCursor {
    segment_file_name: String,
    frame_offset: u64,
    position_ms: u64,
}

#[derive(Debug, Clone)]
#[cfg(test)]
struct StoredSegment {
    file_name: String,
    start_ms: u64,
    end_ms: u64,
    sample_rate: u32,
    channels: u16,
    frames: u64,
    bytes: u64,
}

#[derive(Debug)]
#[cfg(test)]
pub struct TimeshiftStore {
    root: PathBuf,
    segments: VecDeque<StoredSegment>,
    active_file: Option<File>,
    next_segment_id: u64,
    live_head_ms: u64,
    retention_ms: u64,
}

#[cfg(test)]
impl TimeshiftStore {
    pub fn new_session(root: PathBuf, retention_ms: u64) -> Result<Self> {
        clear_root(&root)?;
        fs::create_dir_all(&root)?;

        Ok(Self {
            root,
            segments: VecDeque::new(),
            active_file: None,
            next_segment_id: 0,
            live_head_ms: 0,
            retention_ms,
        })
    }
    pub fn append_pcm(
        &mut self,
        sample_rate: u32,
        channels: u16,
        samples: &[f32],
        now_ms: u64,
    ) -> Result<(u64, u64)> {
        let Some((start_ms, end_ms)) = compute_chunk_timing(
            self.live_head_ms,
            sample_rate,
            channels,
            samples.len(),
            now_ms,
        ) else {
            return Ok((self.live_head_ms, self.live_head_ms));
        };

        let frames = (samples.len() / usize::from(channels)) as u64;

        self.ensure_active_segment(sample_rate, channels, start_ms)?;

        let mut bytes =
            Vec::with_capacity(samples.len() * usize::try_from(BYTES_PER_SAMPLE).unwrap());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }

        if let Some(file) = self.active_file.as_mut() {
            file.write_all(&bytes)?;
        }

        if let Some(segment) = self.segments.back_mut() {
            segment.end_ms = end_ms;
            segment.frames = segment.frames.saturating_add(frames);
            segment.bytes = segment.bytes.saturating_add(bytes.len() as u64);
        }

        self.live_head_ms = end_ms;
        self.prune_old_segments()?;

        Ok((start_ms, end_ms))
    }

    pub fn read_chunk(
        &self,
        cursor_ms: u64,
        max_duration_ms: u64,
    ) -> Result<Option<StoredPcmChunk>> {
        let Some(mut read_cursor) = self.cursor_for_ms(cursor_ms) else {
            return Ok(None);
        };
        self.read_chunk_from(&mut read_cursor, max_duration_ms)
    }

    pub fn cursor_for_ms(&self, cursor_ms: u64) -> Option<PlaybackReadCursor> {
        let segment = self
            .segments
            .iter()
            .find(|segment| cursor_ms < segment.end_ms && cursor_ms >= segment.start_ms)
            .or_else(|| {
                self.segments
                    .iter()
                    .find(|segment| segment.end_ms > cursor_ms)
            })?;

        let sample_rate = u64::from(segment.sample_rate);
        let frame_offset = if cursor_ms <= segment.start_ms {
            0
        } else {
            ((cursor_ms - segment.start_ms).saturating_mul(sample_rate)) / 1000
        }
        .min(segment.frames);

        Some(PlaybackReadCursor {
            segment_file_name: segment.file_name.clone(),
            frame_offset,
            position_ms: cursor_ms.max(segment.start_ms),
        })
    }

    pub fn read_chunk_from(
        &self,
        cursor: &mut PlaybackReadCursor,
        max_duration_ms: u64,
    ) -> Result<Option<StoredPcmChunk>> {
        let (segment_index, segment, frame_offset) = match self.resolve_cursor(cursor) {
            Some(values) => values,
            None => return Ok(None),
        };

        let frames_remaining = segment.frames.saturating_sub(frame_offset);
        if frames_remaining == 0 {
            if let Some(next_segment) = self.segments.get(segment_index + 1) {
                cursor.segment_file_name = next_segment.file_name.clone();
                cursor.frame_offset = 0;
                cursor.position_ms = next_segment.start_ms;
                return self.read_chunk_from(cursor, max_duration_ms);
            }
            return Ok(None);
        }

        let sample_rate = u64::from(segment.sample_rate);
        let channels = u64::from(segment.channels);
        let max_frames = ((max_duration_ms.max(1) * sample_rate) / 1000).max(1);
        let frames_to_read = frames_remaining.min(max_frames);
        let bytes_to_read = frames_to_read
            .saturating_mul(channels)
            .saturating_mul(BYTES_PER_SAMPLE);
        let byte_offset = frame_offset
            .saturating_mul(channels)
            .saturating_mul(BYTES_PER_SAMPLE);

        let mut file = File::open(self.root.join(&segment.file_name))?;
        file.seek(SeekFrom::Start(byte_offset))?;
        let mut bytes = vec![0u8; bytes_to_read as usize];
        file.read_exact(&mut bytes)?;

        let mut samples =
            Vec::with_capacity(bytes.len() / usize::try_from(BYTES_PER_SAMPLE).unwrap());
        for chunk in bytes.chunks_exact(4) {
            samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }

        let chunk_start_ms = segment
            .start_ms
            .saturating_add((frame_offset.saturating_mul(1000)) / sample_rate);
        let next_frame_offset = frame_offset.saturating_add(frames_to_read);
        let chunk_end_ms = segment
            .start_ms
            .saturating_add((next_frame_offset.saturating_mul(1000)) / sample_rate)
            .min(segment.end_ms);

        cursor.frame_offset = next_frame_offset;
        cursor.position_ms = chunk_end_ms;
        if cursor.frame_offset >= segment.frames {
            if let Some(next_segment) = self.segments.get(segment_index + 1) {
                cursor.segment_file_name = next_segment.file_name.clone();
                cursor.frame_offset = 0;
                cursor.position_ms = next_segment.start_ms;
            }
        }

        Ok(Some(StoredPcmChunk {
            channels: segment.channels,
            sample_rate: segment.sample_rate,
            samples,
            start_ms: chunk_start_ms,
            end_ms: chunk_end_ms,
        }))
    }

    pub fn earliest_timestamp_ms(&self) -> Option<u64> {
        self.segments.front().map(|segment| segment.start_ms)
    }
    fn resolve_cursor<'a>(
        &'a self,
        cursor: &mut PlaybackReadCursor,
    ) -> Option<(usize, &'a StoredSegment, u64)> {
        if let Some((idx, segment)) = self
            .segments
            .iter()
            .enumerate()
            .find(|(_, segment)| segment.file_name == cursor.segment_file_name)
        {
            return Some((idx, segment, cursor.frame_offset.min(segment.frames)));
        }

        let (idx, segment) = self
            .segments
            .iter()
            .enumerate()
            .find(|(_, segment)| segment.end_ms > cursor.position_ms)?;
        let sample_rate = u64::from(segment.sample_rate);
        let frame_offset = if cursor.position_ms <= segment.start_ms {
            0
        } else {
            ((cursor.position_ms - segment.start_ms).saturating_mul(sample_rate)) / 1000
        }
        .min(segment.frames);

        cursor.segment_file_name = segment.file_name.clone();
        cursor.frame_offset = frame_offset;
        cursor.position_ms = cursor.position_ms.max(segment.start_ms);

        Some((idx, segment, frame_offset))
    }

    fn ensure_active_segment(
        &mut self,
        sample_rate: u32,
        channels: u16,
        start_ms: u64,
    ) -> Result<()> {
        let needs_rotate = match self.segments.back() {
            Some(segment) => {
                segment.sample_rate != sample_rate
                    || segment.channels != channels
                    || segment.bytes >= MAX_SEGMENT_BYTES
            }
            None => true,
        };

        if !needs_rotate {
            return Ok(());
        }

        let file_name = format!("segment-{:06}.pcm", self.next_segment_id);
        self.next_segment_id = self.next_segment_id.saturating_add(1);
        let path = self.root.join(&file_name);
        let file = OpenOptions::new().create(true).append(true).open(path)?;

        self.segments.push_back(StoredSegment {
            file_name,
            start_ms,
            end_ms: start_ms,
            sample_rate,
            channels,
            frames: 0,
            bytes: 0,
        });
        self.active_file = Some(file);
        Ok(())
    }

    fn prune_old_segments(&mut self) -> Result<()> {
        let prune_before = self.live_head_ms.saturating_sub(self.retention_ms);

        while self.segments.len() > 1 {
            let should_remove = self
                .segments
                .front()
                .map(|segment| segment.end_ms < prune_before)
                .unwrap_or(false);
            if !should_remove {
                break;
            }

            if let Some(removed) = self.segments.pop_front() {
                match fs::remove_file(self.root.join(removed.file_name)) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err.into()),
                }
            }
        }

        Ok(())
    }
}

pub fn clear_root(root: &Path) -> Result<()> {
    match fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::{TimeshiftStore, RETENTION_MS};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("listenmoe-{name}-{unique}"))
    }

    fn samples(frames: usize, channels: u16) -> Vec<f32> {
        (0..(frames * usize::from(channels)))
            .map(|i| i as f32 / 32.0)
            .collect()
    }

    #[test]
    fn rotates_segments_when_file_size_limit_is_reached() {
        let root = temp_root("store-rotate");
        let mut store = TimeshiftStore::new_session(root.clone(), RETENTION_MS).expect("store");
        let chunk = vec![0.0f32; 1_000_000];
        for offset in 0..20 {
            store
                .append_pcm(48_000, 1, &chunk, 1_000 + offset * 1_000)
                .expect("append");
        }
        store
            .append_pcm(48_000, 1, &[0.5; 64], 2_000)
            .expect("second append");

        let segment_count = std::fs::read_dir(&root)
            .expect("read_dir")
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext == "pcm")
            })
            .count();
        assert!(segment_count >= 2);
    }

    #[test]
    fn prunes_old_segments_after_retention_window() {
        let root = temp_root("store-prune");
        let mut store = TimeshiftStore::new_session(root.clone(), 2_000).expect("store");
        store
            .append_pcm(1_000, 1, &samples(1_000, 1), 1_000)
            .expect("append old");
        store
            .append_pcm(1_000, 1, &samples(1_000, 1), 4_500)
            .expect("append new");

        assert!(store.earliest_timestamp_ms().expect("missing earliest") >= 2_500);
    }

    #[test]
    fn reads_back_audio_at_persisted_cursor() {
        let root = temp_root("store-read");
        let mut store = TimeshiftStore::new_session(root, RETENTION_MS).expect("store");
        store
            .append_pcm(1_000, 2, &samples(1_000, 2), 1_000)
            .expect("append");

        let chunk = store
            .read_chunk(500, 250)
            .expect("read")
            .expect("missing chunk");
        assert_eq!(chunk.channels, 2);
        assert_eq!(chunk.sample_rate, 1_000);
        assert_eq!(chunk.samples.len(), 500);
        assert!(chunk.end_ms > chunk.start_ms);
    }

    #[test]
    fn sequential_cursor_reads_do_not_drop_or_duplicate_samples() {
        let root = temp_root("store-sequential");
        let mut store = TimeshiftStore::new_session(root, RETENTION_MS).expect("store");
        let original = samples(44_100, 1);
        store
            .append_pcm(44_100, 1, &original, 1_000)
            .expect("append");

        let mut cursor = store.cursor_for_ms(0).expect("cursor");
        let mut recovered = Vec::new();

        while let Some(chunk) = store
            .read_chunk_from(&mut cursor, 137)
            .expect("read sequential chunk")
        {
            recovered.extend_from_slice(&chunk.samples);
            if recovered.len() >= original.len() {
                break;
            }
        }

        assert_eq!(recovered.len(), original.len());
        assert_eq!(recovered, original);
    }
}
