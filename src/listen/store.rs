use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::Result;

pub const RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const GAP_THRESHOLD_MS: u64 = 1_500;
const MAX_SEGMENT_BYTES: u64 = 512 * 1024 * 1024;
const BYTES_PER_SAMPLE: u64 = 4;

#[derive(Debug, Clone)]
pub struct StoredPcmChunk {
    pub channels: u16,
    pub sample_rate: u32,
    pub samples: Vec<f32>,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone)]
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
pub struct TimeshiftStore {
    root: PathBuf,
    segments: VecDeque<StoredSegment>,
    active_file: Option<File>,
    next_segment_id: u64,
    live_head_ms: u64,
    retention_ms: u64,
}

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

    pub fn clear(&mut self) -> Result<()> {
        self.active_file = None;
        self.segments.clear();
        self.live_head_ms = 0;
        clear_root(&self.root)?;
        fs::create_dir_all(&self.root)?;
        Ok(())
    }

    pub fn append_pcm(
        &mut self,
        sample_rate: u32,
        channels: u16,
        samples: &[f32],
        now_ms: u64,
    ) -> Result<(u64, u64)> {
        if samples.is_empty() || sample_rate == 0 || channels == 0 {
            return Ok((self.live_head_ms, self.live_head_ms));
        }

        let frames = (samples.len() / usize::from(channels)) as u64;
        if frames == 0 {
            return Ok((self.live_head_ms, self.live_head_ms));
        }

        let duration_ms = ((frames.saturating_mul(1000)) / u64::from(sample_rate)).max(1);
        let expected_start_ms = if self.live_head_ms == 0 {
            now_ms.saturating_sub(duration_ms)
        } else {
            self.live_head_ms
        };
        let wall_start_ms = now_ms.saturating_sub(duration_ms);
        let start_ms = if self.live_head_ms != 0
            && wall_start_ms > expected_start_ms.saturating_add(GAP_THRESHOLD_MS)
        {
            wall_start_ms
        } else {
            expected_start_ms
        };
        let end_ms = start_ms.saturating_add(duration_ms);

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
        let segment = match self
            .segments
            .iter()
            .find(|segment| cursor_ms < segment.end_ms && cursor_ms >= segment.start_ms)
        {
            Some(segment) => segment,
            None => return Ok(None),
        };

        let sample_rate = u64::from(segment.sample_rate);
        let channels = u64::from(segment.channels);
        let frame_offset = if cursor_ms <= segment.start_ms {
            0
        } else {
            ((cursor_ms - segment.start_ms).saturating_mul(sample_rate)) / 1000
        }
        .min(segment.frames);
        let frames_remaining = segment.frames.saturating_sub(frame_offset);
        if frames_remaining == 0 {
            return Ok(None);
        }

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
        let chunk_end_ms = chunk_start_ms
            .saturating_add((frames_to_read.saturating_mul(1000)) / sample_rate)
            .min(segment.end_ms);

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

    pub fn next_available_timestamp_ms(&self, cursor_ms: u64) -> Option<u64> {
        self.segments
            .iter()
            .find(|segment| segment.end_ms > cursor_ms)
            .map(|segment| segment.start_ms.max(cursor_ms))
    }

    pub fn clamp_cursor_ms(&self, cursor_ms: u64) -> u64 {
        match self.earliest_timestamp_ms() {
            Some(oldest) => cursor_ms.max(oldest),
            None => cursor_ms,
        }
    }

    pub fn live_head_ms(&self) -> u64 {
        self.live_head_ms
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
}
