use reqwest::blocking::Client;
use rodio::{buffer::SamplesBuffer, queue, DeviceSinkBuilder, Player, Source};
use std::collections::VecDeque;
use std::num::{NonZeroU16, NonZeroU32};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::http_source::HttpSource;
use crate::log::{is_verbose, now_string};
use crate::station::Station;

use super::clock::PlaybackClock;
use super::store::{
    clear_root, compute_chunk_timing, PlaybackReadCursor, StoredPcmChunk, TimeshiftStore,
    RETENTION_MS,
};
use super::viz::{
    apply_spectrum_snapshot, build_spectrum_snapshot, clear_spectrum, decode_and_process_packet,
    make_fft_state, reset_fft_state, DecodeState, FftVizState, PacketOutcome, VizParams,
};
use super::{Control, Result};

#[derive(Debug, Clone, Copy)]
enum RunOutcome {
    Stop,
    Reconnect,
}

#[derive(Debug, Clone, Copy)]
enum LiveDirectOutcome {
    Stop,
    TransitionToBuffered,
}

#[derive(Debug, Clone, Copy)]
enum LivePlayerCommand {
    Reset,
    Stop,
}

enum OutputCommandResult {
    Continue,
    Stop,
    RestartPlayback,
}

const OUTPUT_CHUNK_MS: u64 = 1_000;
const OUTPUT_MIN_HEADROOM_MS: u64 = 8_000;
const OUTPUT_WAIT_TIMEOUT_MS: u64 = 25;
const LIVE_BUFFER_MS: u64 = RETENTION_MS;
const LIVE_CLOCK_FLUSH_MS: u64 = 250;
const PLAYBACK_PREFILL_CHUNKS: usize = 8;
const PLAYBACK_QUEUE_MAX_CHUNKS: usize = 16;

#[derive(Default)]
struct PlaybackNotifier {
    generation: Mutex<u64>,
    condvar: Condvar,
}

impl PlaybackNotifier {
    fn notify(&self) {
        let mut generation = self.generation.lock().expect("playback notifier poisoned");
        *generation = generation.saturating_add(1);
        self.condvar.notify_all();
    }

    fn wait_for_change(&self, last_seen: &mut u64, stop_requested: &Arc<AtomicBool>) {
        let generation = self.generation.lock().expect("playback notifier poisoned");
        let generation = self
            .condvar
            .wait_while(generation, |generation| {
                *generation == *last_seen && !stop_requested.load(Ordering::Relaxed)
            })
            .expect("playback notifier poisoned");
        *last_seen = *generation;
    }
}

#[derive(Debug, Clone, Copy)]
enum PlaybackEvent {
    RestartRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaybackQueueCloseReason {
    Stopped,
    RestartRequired,
}

struct QueuedPlaybackChunk {
    audio: StoredPcmChunk,
    spectrum_snapshot: Vec<u32>,
}

impl QueuedPlaybackChunk {
    fn from_audio(audio: StoredPcmChunk, fft_state: &mut FftVizState, viz: VizParams) -> Self {
        let spectrum_snapshot = build_spectrum_snapshot(
            &audio.samples,
            audio.channels,
            audio.sample_rate,
            fft_state,
            viz,
        );

        Self {
            audio,
            spectrum_snapshot,
        }
    }
}

#[derive(Default)]
struct PlaybackQueueState {
    chunks: VecDeque<QueuedPlaybackChunk>,
    close_reason: Option<PlaybackQueueCloseReason>,
}

#[derive(Default)]
struct PlaybackQueue {
    state: Mutex<PlaybackQueueState>,
    condvar: Condvar,
}

impl PlaybackQueue {
    fn push_prefilled(&self, chunk: QueuedPlaybackChunk) {
        let mut state = self.state.lock().expect("playback queue poisoned");
        if state.close_reason.is_none() {
            state.chunks.push_back(chunk);
            self.condvar.notify_all();
        }
    }

    fn push_blocking(&self, chunk: QueuedPlaybackChunk, stop_requested: &Arc<AtomicBool>) -> bool {
        let state = self.state.lock().expect("playback queue poisoned");
        let mut state = self
            .condvar
            .wait_while(state, |state| {
                state.close_reason.is_none()
                    && state.chunks.len() >= PLAYBACK_QUEUE_MAX_CHUNKS
                    && !stop_requested.load(Ordering::Relaxed)
            })
            .expect("playback queue poisoned");

        if state.close_reason.is_some() || stop_requested.load(Ordering::Relaxed) {
            return false;
        }

        state.chunks.push_back(chunk);
        self.condvar.notify_all();
        true
    }

    fn pop_blocking(&self, stop_requested: &Arc<AtomicBool>) -> Option<QueuedPlaybackChunk> {
        let state = self.state.lock().expect("playback queue poisoned");
        let mut state = self
            .condvar
            .wait_while(state, |state| {
                state.chunks.is_empty()
                    && state.close_reason.is_none()
                    && !stop_requested.load(Ordering::Relaxed)
            })
            .expect("playback queue poisoned");

        let chunk = state.chunks.pop_front();
        if chunk.is_some() {
            self.condvar.notify_all();
        }
        chunk
    }

    fn close(&self, reason: PlaybackQueueCloseReason) {
        let mut state = self.state.lock().expect("playback queue poisoned");
        state.close_reason.get_or_insert(reason);
        self.condvar.notify_all();
    }
}

#[derive(Default)]
struct LiveChunkBufferState {
    chunks: VecDeque<LiveChunkEntry>,
    generation: u64,
    next_chunk_id: u64,
}

#[derive(Debug, Clone)]
struct LiveChunkEntry {
    id: u64,
    audio: StoredPcmChunk,
}

#[derive(Debug, Clone)]
struct LiveReadCursor {
    chunk_id: u64,
    sample_offset: usize,
    position_ms: u64,
}

#[derive(Default)]
struct LiveChunkBuffer {
    state: Mutex<LiveChunkBufferState>,
    condvar: Condvar,
}

impl LiveChunkBuffer {
    fn push(&self, chunk: StoredPcmChunk) {
        let mut state = self.state.lock().expect("live chunk buffer poisoned");
        let live_head_ms = chunk.end_ms;
        let chunk_id = state.next_chunk_id;
        state.next_chunk_id = state.next_chunk_id.saturating_add(1);
        state.chunks.push_back(LiveChunkEntry {
            id: chunk_id,
            audio: chunk,
        });

        let prune_before = live_head_ms.saturating_sub(LIVE_BUFFER_MS);
        while state.chunks.len() > 1 {
            let should_remove = state
                .chunks
                .front()
                .map(|chunk| chunk.audio.end_ms <= prune_before)
                .unwrap_or(false);
            if !should_remove {
                break;
            }
            state.chunks.pop_front();
        }

        state.generation = state.generation.saturating_add(1);
        self.condvar.notify_all();
    }

    fn earliest_ms(&self) -> Option<u64> {
        self.state
            .lock()
            .expect("live chunk buffer poisoned")
            .chunks
            .front()
            .map(|chunk| chunk.audio.start_ms)
    }

    fn cursor_for_ms(&self, cursor_ms: u64) -> Option<LiveReadCursor> {
        let state = self.state.lock().expect("live chunk buffer poisoned");
        let entry = state
            .chunks
            .iter()
            .find(|entry| cursor_ms < entry.audio.end_ms && cursor_ms >= entry.audio.start_ms)
            .or_else(|| {
                state
                    .chunks
                    .iter()
                    .find(|entry| entry.audio.end_ms > cursor_ms)
            })?;

        let channels = usize::from(entry.audio.channels);
        if channels == 0 || entry.audio.sample_rate == 0 {
            return None;
        }

        let total_frames = entry.audio.samples.len() / channels;
        if total_frames == 0 {
            return None;
        }

        let frame_offset = if cursor_ms <= entry.audio.start_ms {
            0
        } else {
            (((cursor_ms - entry.audio.start_ms)
                .saturating_mul(u64::from(entry.audio.sample_rate)))
                / 1_000)
                .min(total_frames as u64)
        } as usize;

        let sample_offset = frame_offset.saturating_mul(channels);
        let position_ms = entry.audio.start_ms.saturating_add(
            (u64::try_from(frame_offset)
                .unwrap_or(u64::MAX)
                .saturating_mul(1_000))
                / u64::from(entry.audio.sample_rate),
        );

        Some(LiveReadCursor {
            chunk_id: entry.id,
            sample_offset,
            position_ms,
        })
    }

    fn read_chunk_from(
        &self,
        cursor: &mut LiveReadCursor,
        max_duration_ms: u64,
    ) -> Option<StoredPcmChunk> {
        let state = self.state.lock().expect("live chunk buffer poisoned");
        let (mut entry_index, entry, mut sample_offset) = Self::resolve_cursor(&state, cursor)?;

        let channels = usize::from(entry.audio.channels);
        if channels == 0 || entry.audio.sample_rate == 0 {
            return None;
        }

        let sample_rate = u64::from(entry.audio.sample_rate);
        let max_frames = usize::try_from(((max_duration_ms.max(1) * sample_rate) / 1_000).max(1))
            .unwrap_or(usize::MAX);
        let frame_offset = sample_offset / channels;
        let chunk_start_ms = entry.audio.start_ms.saturating_add(
            (u64::try_from(frame_offset)
                .unwrap_or(u64::MAX)
                .saturating_mul(1_000))
                / sample_rate,
        );
        let mut merged = StoredPcmChunk {
            channels: entry.audio.channels,
            sample_rate: entry.audio.sample_rate,
            samples: Vec::new(),
            start_ms: chunk_start_ms,
            end_ms: chunk_start_ms,
        };

        loop {
            let current = state.chunks.get(entry_index)?;
            if current.audio.channels != merged.channels
                || current.audio.sample_rate != merged.sample_rate
                || (!merged.samples.is_empty() && current.audio.start_ms != merged.end_ms)
            {
                cursor.chunk_id = current.id;
                cursor.sample_offset = 0;
                cursor.position_ms = current.audio.start_ms;
                break;
            }

            let total_frames = current.audio.samples.len() / channels;
            let entry_frame_offset = sample_offset / channels;
            let frames_remaining = total_frames.saturating_sub(entry_frame_offset);
            let merged_frames = merged.samples.len() / channels;
            let frames_needed = max_frames.saturating_sub(merged_frames);

            if frames_remaining == 0 {
                if let Some(next) = state.chunks.get(entry_index + 1) {
                    cursor.chunk_id = next.id;
                    cursor.sample_offset = 0;
                    cursor.position_ms = next.audio.start_ms;
                    entry_index = entry_index.saturating_add(1);
                    sample_offset = 0;
                    continue;
                } else {
                    cursor.chunk_id = current.id;
                    cursor.sample_offset = current.audio.samples.len();
                    cursor.position_ms = current.audio.end_ms;
                }
                break;
            }

            if frames_needed == 0 {
                cursor.chunk_id = current.id;
                cursor.sample_offset = sample_offset;
                cursor.position_ms = merged.end_ms;
                break;
            }

            let frames_to_take = frames_remaining.min(frames_needed);
            let start_sample = entry_frame_offset.saturating_mul(channels);
            let end_sample = start_sample.saturating_add(frames_to_take.saturating_mul(channels));
            merged
                .samples
                .extend_from_slice(&current.audio.samples[start_sample..end_sample]);

            let next_frame_offset = entry_frame_offset.saturating_add(frames_to_take);
            merged.end_ms = current.audio.start_ms.saturating_add(
                (u64::try_from(next_frame_offset)
                    .unwrap_or(u64::MAX)
                    .saturating_mul(1_000))
                    / sample_rate,
            );

            if next_frame_offset < total_frames {
                cursor.chunk_id = current.id;
                cursor.sample_offset = next_frame_offset.saturating_mul(channels);
                cursor.position_ms = merged.end_ms;
                break;
            }

            sample_offset = 0;
            entry_index = entry_index.saturating_add(1);
            if let Some(next) = state.chunks.get(entry_index) {
                if next.audio.channels != merged.channels
                    || next.audio.sample_rate != merged.sample_rate
                    || next.audio.start_ms != merged.end_ms
                    || (merged.samples.len() / channels) >= max_frames
                {
                    cursor.chunk_id = next.id;
                    cursor.sample_offset = 0;
                    cursor.position_ms = next.audio.start_ms;
                    break;
                }
            } else {
                cursor.chunk_id = current.id;
                cursor.sample_offset = current.audio.samples.len();
                cursor.position_ms = merged.end_ms;
                break;
            }
        }

        if merged.samples.is_empty() {
            return None;
        }

        Some(merged)
    }

    fn wait_for_change(&self, last_seen: &mut u64, stop_requested: &Arc<AtomicBool>) {
        let generation = self.state.lock().expect("live chunk buffer poisoned");
        let generation = self
            .condvar
            .wait_while(generation, |state| {
                state.generation == *last_seen && !stop_requested.load(Ordering::Relaxed)
            })
            .expect("live chunk buffer poisoned");
        *last_seen = generation.generation;
    }

    fn wake(&self) {
        let mut state = self.state.lock().expect("live chunk buffer poisoned");
        state.generation = state.generation.saturating_add(1);
        self.condvar.notify_all();
    }

    fn resolve_cursor<'a>(
        state: &'a LiveChunkBufferState,
        cursor: &mut LiveReadCursor,
    ) -> Option<(usize, &'a LiveChunkEntry, usize)> {
        if let Some((idx, entry)) = state
            .chunks
            .iter()
            .enumerate()
            .find(|(_, entry)| entry.id == cursor.chunk_id)
        {
            return Some((
                idx,
                entry,
                cursor.sample_offset.min(entry.audio.samples.len()),
            ));
        }

        let (idx, entry) = state
            .chunks
            .iter()
            .enumerate()
            .find(|(_, entry)| entry.audio.end_ms > cursor.position_ms)?;

        let channels = usize::from(entry.audio.channels);
        if channels == 0 || entry.audio.sample_rate == 0 {
            return None;
        }

        let total_frames = entry.audio.samples.len() / channels;
        let frame_offset = if cursor.position_ms <= entry.audio.start_ms {
            0
        } else {
            (((cursor.position_ms - entry.audio.start_ms)
                .saturating_mul(u64::from(entry.audio.sample_rate)))
                / 1_000)
                .min(total_frames as u64)
        } as usize;
        let sample_offset = frame_offset.saturating_mul(channels);
        let position_ms = entry.audio.start_ms.saturating_add(
            (u64::try_from(frame_offset)
                .unwrap_or(u64::MAX)
                .saturating_mul(1_000))
                / u64::from(entry.audio.sample_rate),
        );

        cursor.chunk_id = entry.id;
        cursor.sample_offset = sample_offset;
        cursor.position_ms = position_ms;

        Some((idx, entry, sample_offset))
    }
}

struct AbortableSource<I> {
    inner: I,
    playback_generation: Arc<AtomicU64>,
    expected_generation: u64,
}

impl<I> AbortableSource<I> {
    fn new(inner: I, playback_generation: Arc<AtomicU64>, expected_generation: u64) -> Self {
        Self {
            inner,
            playback_generation,
            expected_generation,
        }
    }
}

impl<I> Iterator for AbortableSource<I>
where
    I: Source,
{
    type Item = I::Item;

    fn next(&mut self) -> Option<Self::Item> {
        if self.playback_generation.load(Ordering::Relaxed) != self.expected_generation {
            return None;
        }

        self.inner.next()
    }
}

impl<I> Source for AbortableSource<I>
where
    I: Source,
{
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> rodio::ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, pos: Duration) -> std::result::Result<(), rodio::source::SeekError> {
        self.inner.try_seek(pos)
    }
}

fn drain_finished_chunks(
    in_flight: &mut VecDeque<(mpsc::Receiver<()>, u64)>,
    clock: &Arc<PlaybackClock>,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
) {
    loop {
        let finished = match in_flight.front() {
            Some((done_rx, _)) => match done_rx.try_recv() {
                Ok(()) | Err(mpsc::TryRecvError::Disconnected) => true,
                Err(mpsc::TryRecvError::Empty) => false,
            },
            None => false,
        };

        if !finished {
            break;
        }

        let (_, end_ms) = in_flight
            .pop_front()
            .expect("front element must exist when marked finished");
        clock.set_playback_cursor_ms(end_ms);
        if in_flight.is_empty() {
            clear_spectrum(spectrum_bits);
        }
    }
}

fn wait_for_oldest_chunk(
    in_flight: &mut VecDeque<(mpsc::Receiver<()>, u64)>,
    clock: &Arc<PlaybackClock>,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
    stop_requested: &Arc<AtomicBool>,
    playback_generation: &Arc<AtomicU64>,
    expected_generation: u64,
) -> bool {
    loop {
        if stop_requested.load(Ordering::Relaxed)
            || playback_generation.load(Ordering::Relaxed) != expected_generation
        {
            return false;
        }

        let result = match in_flight.front() {
            Some((done_rx, _)) => {
                done_rx.recv_timeout(Duration::from_millis(OUTPUT_WAIT_TIMEOUT_MS))
            }
            None => return true,
        };

        match result {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let (_, end_ms) = in_flight
                    .pop_front()
                    .expect("front element must exist when marked finished");
                clock.set_playback_cursor_ms(end_ms);
                if in_flight.is_empty() {
                    clear_spectrum(spectrum_bits);
                }
                return true;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn run_playback_output(
    output: Arc<queue::SourcesQueueInput>,
    playback_queue: Arc<PlaybackQueue>,
    stop_requested: Arc<AtomicBool>,
    clock: Arc<PlaybackClock>,
    spectrum_bits: Arc<Vec<AtomicU32>>,
    playback_generation: Arc<AtomicU64>,
    expected_generation: u64,
) {
    let mut in_flight = VecDeque::new();

    loop {
        drain_finished_chunks(&mut in_flight, &clock, &spectrum_bits);

        if stop_requested.load(Ordering::Relaxed)
            || playback_generation.load(Ordering::Relaxed) != expected_generation
        {
            break;
        }

        if in_flight.len() >= PLAYBACK_QUEUE_MAX_CHUNKS {
            if !wait_for_oldest_chunk(
                &mut in_flight,
                &clock,
                &spectrum_bits,
                &stop_requested,
                &playback_generation,
                expected_generation,
            ) {
                break;
            }
            continue;
        }

        let Some(chunk) = playback_queue.pop_blocking(&stop_requested) else {
            break;
        };

        if playback_generation.load(Ordering::Relaxed) != expected_generation {
            break;
        }

        let start_ms = chunk.audio.start_ms;
        let end_ms = chunk.audio.end_ms;
        let spectrum_snapshot = chunk.spectrum_snapshot;
        let source_clock = clock.clone();
        let source_spectrum_bits = spectrum_bits.clone();
        let source_generation = playback_generation.clone();
        let mut announced = false;

        let source = SamplesBuffer::new(
            rodio::ChannelCount::new(chunk.audio.channels)
                .expect("timeshift chunk must have non-zero channels"),
            rodio::SampleRate::new(chunk.audio.sample_rate)
                .expect("timeshift chunk must have non-zero sample rate"),
            chunk.audio.samples,
        )
        .track_position()
        .periodic_access(Duration::from_millis(20), move |src| {
            if !announced {
                apply_spectrum_snapshot(&source_spectrum_bits, &spectrum_snapshot);
                announced = true;
            }

            let pos_ms = src.get_pos().as_millis() as u64;
            source_clock.set_playback_cursor_ms(start_ms.saturating_add(pos_ms).min(end_ms));
        });

        let done_rx = output.append_with_signal(AbortableSource::new(
            source,
            source_generation,
            expected_generation,
        ));
        in_flight.push_back((done_rx, end_ms));
    }

    output.clear();
    clear_spectrum(&spectrum_bits);
}

fn append_samples_in_chunks(sink: &Player, channels: u16, sample_rate: u32, samples: &[f32]) {
    const CHUNK_MS: u32 = 10;

    let ch = usize::from(channels);
    if ch == 0 || sample_rate == 0 {
        return;
    }

    let Some(channels) = NonZeroU16::new(channels) else {
        return;
    };
    let Some(sample_rate) = NonZeroU32::new(sample_rate) else {
        return;
    };

    let frames_per_chunk = (sample_rate.get() * CHUNK_MS / 1000).max(1) as usize;
    let samples_per_chunk = frames_per_chunk * ch;

    for chunk in samples.chunks(samples_per_chunk) {
        sink.append(SamplesBuffer::new(channels, sample_rate, chunk.to_vec()));
    }
}

fn reset_direct_player(direct_player: &mut Player, direct_mixer: &rodio::mixer::Mixer) {
    direct_player.stop();
    *direct_player = Player::connect_new(direct_mixer);
}

fn buffered_playback_start_ms(live_head_ms: u64, floor_ms: Option<u64>) -> u64 {
    let buffered = live_head_ms.saturating_sub(OUTPUT_MIN_HEADROOM_MS);
    match floor_ms {
        Some(floor_ms) => buffered.max(floor_ms).min(live_head_ms),
        None => buffered,
    }
}

fn build_client() -> Result<Client> {
    Ok(Client::builder()
        .pool_max_idle_per_host(0)
        .connect_timeout(Duration::from_secs(5))
        .build()?)
}

fn build_useragent() -> String {
    let platform = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "other"
    };

    format!(
        "{}-v{}-{}",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        platform
    )
}

fn handle_live_direct_control(
    rx: &mpsc::Receiver<Control>,
    sink: &Player,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
) -> Result<Option<LiveDirectOutcome>> {
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            Control::Stop => {
                if is_verbose() {
                    println!(
                        "[{}] Stop requested, shutting down live stream.",
                        now_string()
                    );
                }
                sink.stop();
                clear_spectrum(spectrum_bits);
                return Ok(Some(LiveDirectOutcome::Stop));
            }
            Control::Pause => {
                if is_verbose() {
                    println!(
                        "[{}] Pausing live playback, switching to timeshift.",
                        now_string()
                    );
                }
                sink.stop();
                clear_spectrum(spectrum_bits);
                return Ok(Some(LiveDirectOutcome::TransitionToBuffered));
            }
            Control::Resume => {}
        }
    }

    Ok(None)
}

fn open_stream(
    url: &str,
    client: &Client,
    useragent: &str,
    format_opts: &FormatOptions,
    metadata_opts: &MetadataOptions,
    decoder_opts: &DecoderOptions,
) -> Result<(
    Box<dyn symphonia::core::formats::FormatReader>,
    u32,
    Box<dyn symphonia::core::codecs::Decoder>,
)> {
    if is_verbose() {
        println!("[{}] Connecting to {url}…", now_string());
    }

    let response = client.get(url).header("User-Agent", useragent).send()?;
    if is_verbose() {
        println!("[{}] HTTP status: {}", now_string(), response.status());
    }

    if !response.status().is_success() {
        return Err(format!("HTTP status {}", response.status()).into());
    }

    let http_source = HttpSource { inner: response };
    let mss = MediaSourceStream::new(Box::new(http_source), Default::default());
    let hint = Hint::new();

    let probed = symphonia::default::get_probe().format(&hint, mss, format_opts, metadata_opts)?;
    let format = probed.format;

    let (track_id, decoder) = {
        let track = format
            .tracks()
            .iter()
            .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or_else(|| "no supported audio tracks".to_string())?;

        let track_id = track.id;
        let decoder = symphonia::default::get_codecs().make(&track.codec_params, decoder_opts)?;
        (track_id, decoder)
    };

    Ok((format, track_id, decoder))
}

fn run_direct_live_until_pause(
    station: Station,
    rx: &mpsc::Receiver<Control>,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
    clock: &Arc<PlaybackClock>,
    mixer: &rodio::mixer::Mixer,
) -> Result<LiveDirectOutcome> {
    let primary = station.stream_url().to_string();
    let fallback = station.stream_fallback_url().to_string();
    let mut use_fallback = false;

    let mut client = build_client()?;
    let useragent = build_useragent();

    let format_opts: FormatOptions = Default::default();
    let metadata_opts: MetadataOptions = Default::default();
    let decoder_opts: DecoderOptions = Default::default();

    let mut sink = Player::connect_new(mixer);
    let mut fft_state = make_fft_state(spectrum_bits.len());
    let mut live_clock = LiveClockTracker::new(clock.live_head_ms());
    let viz = live_viz_params();

    loop {
        if let Some(outcome) = handle_live_direct_control(rx, &sink, spectrum_bits)? {
            live_clock.flush_now(clock);
            return Ok(outcome);
        }

        let url: &str = if use_fallback { &fallback } else { &primary };
        let (mut format, mut track_id, mut decoder) = match open_stream(
            url,
            &client,
            &useragent,
            &format_opts,
            &metadata_opts,
            &decoder_opts,
        ) {
            Ok(parts) => parts,
            Err(err) => {
                eprintln!("connect/probe error on {url}: {err}");
                if !fallback.is_empty() {
                    use_fallback = !use_fallback;
                }
                client = build_client()?;
                continue;
            }
        };

        sink.stop();
        sink = Player::connect_new(mixer);
        live_clock.mark_reconnect();
        reset_fft_state(
            &mut fft_state.mono_ring,
            &mut fft_state.bars_smooth,
            &mut fft_state.bar_peak,
            spectrum_bits,
        );

        let mut decode_state = DecodeState {
            sample_buf: None,
            channels: 0,
            sample_rate: 0,
        };

        loop {
            if let Some(outcome) = handle_live_direct_control(rx, &sink, spectrum_bits)? {
                live_clock.flush_now(clock);
                return Ok(outcome);
            }

            let packet = match format.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::ResetRequired) => {
                    if is_verbose() {
                        println!(
                            "[{}] Stream reset, reconfiguring live decoder…",
                            now_string()
                        );
                    }

                    let new_track = format
                        .tracks()
                        .iter()
                        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
                        .ok_or_else(|| "no supported audio tracks after reset".to_string())?;

                    track_id = new_track.id;
                    decoder = symphonia::default::get_codecs()
                        .make(&new_track.codec_params, &decoder_opts)?;
                    decode_state.sample_buf = None;
                    reset_fft_state(
                        &mut fft_state.mono_ring,
                        &mut fft_state.bars_smooth,
                        &mut fft_state.bar_peak,
                        spectrum_bits,
                    );
                    continue;
                }
                Err(err) => {
                    eprintln!("Error reading live packet: {err:?}");
                    live_clock.flush_now(clock);
                    break;
                }
            };

            let (outcome, audio) = decode_and_process_packet(
                &packet,
                &mut format,
                &mut track_id,
                &mut decoder,
                &decoder_opts,
                true,
                spectrum_bits,
                &mut decode_state,
                &mut fft_state,
                viz,
            )?;

            match outcome {
                PacketOutcome::Continue => {}
                PacketOutcome::Reconnect => break,
                PacketOutcome::SpecChanged => {
                    sink.stop();
                    reset_fft_state(
                        &mut fft_state.mono_ring,
                        &mut fft_state.bars_smooth,
                        &mut fft_state.bar_peak,
                        spectrum_bits,
                    );
                    continue;
                }
            }

            if let Some((channels, sample_rate, samples)) = audio {
                append_samples_in_chunks(&sink, channels, sample_rate, &samples);
                if live_clock
                    .advance(sample_rate, channels, samples.len())
                    .is_some()
                {
                    live_clock.flush_if_due(clock);
                }
            }
        }

        if !fallback.is_empty() {
            use_fallback = !use_fallback;
        }
    }
}

fn handle_output_command(
    cmd: Control,
    paused: &mut bool,
    stop_requested: &Arc<AtomicBool>,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
) -> Result<OutputCommandResult> {
    match cmd {
        Control::Stop => {
            if is_verbose() {
                println!("[{}] Stop requested, shutting down stream.", now_string());
            }
            stop_requested.store(true, Ordering::Relaxed);
            clear_spectrum(spectrum_bits);
            return Ok(OutputCommandResult::Stop);
        }
        Control::Pause => {
            if !*paused {
                if is_verbose() {
                    println!("[{}] Pausing playback.", now_string());
                }
                *paused = true;
                clear_spectrum(spectrum_bits);
                return Ok(OutputCommandResult::RestartPlayback);
            }
            clear_spectrum(spectrum_bits);
        }
        Control::Resume => {
            if *paused {
                if is_verbose() {
                    println!("[{}] Resuming playback.", now_string());
                }
                *paused = false;
                return Ok(OutputCommandResult::RestartPlayback);
            }
        }
    }

    Ok(OutputCommandResult::Continue)
}

fn handle_output_control(
    rx: &mpsc::Receiver<Control>,
    paused: &mut bool,
    stop_requested: &Arc<AtomicBool>,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
) -> Result<OutputCommandResult> {
    while let Ok(cmd) = rx.try_recv() {
        match handle_output_command(cmd, paused, stop_requested, spectrum_bits)? {
            OutputCommandResult::Continue => {}
            result => return Ok(result),
        }
    }

    Ok(OutputCommandResult::Continue)
}

struct PlaybackStart {
    queue: Arc<PlaybackQueue>,
    prefetch_handle: thread::JoinHandle<()>,
    output_handle: thread::JoinHandle<()>,
}

enum StoreCommand {
    Append {
        sample_rate: u32,
        channels: u16,
        samples: Arc<[f32]>,
        now_ms: u64,
    },
}

enum PrefetchChunkResult {
    Ready(QueuedPlaybackChunk, PlaybackReadCursor),
    Pending,
    RestartRequired,
}

fn playback_viz_params() -> VizParams {
    VizParams {
        peak_attack: 0.08,
        peak_release: 0.998,
        sensitivity: 1.25,
        curve: 0.75,
    }
}

fn live_viz_params() -> VizParams {
    VizParams {
        peak_attack: 0.08,
        peak_release: 0.998,
        sensitivity: 1.25,
        curve: 0.75,
    }
}

#[derive(Debug, Clone, Copy)]
struct LiveClockTracker {
    live_head_ms: u64,
    last_flushed_ms: u64,
    resync_from_wall: bool,
}

impl LiveClockTracker {
    fn new(seed_live_head_ms: u64) -> Self {
        Self {
            live_head_ms: seed_live_head_ms,
            last_flushed_ms: seed_live_head_ms,
            resync_from_wall: seed_live_head_ms == 0,
        }
    }

    fn mark_reconnect(&mut self) {
        self.resync_from_wall = true;
    }

    fn advance(&mut self, sample_rate: u32, channels: u16, sample_count: usize) -> Option<u64> {
        if sample_rate == 0 || channels == 0 || sample_count == 0 {
            return None;
        }

        let frames = sample_count / usize::from(channels);
        if frames == 0 {
            return None;
        }

        let duration_ms =
            ((u64::try_from(frames).unwrap_or(u64::MAX) * 1_000) / u64::from(sample_rate)).max(1);

        if self.resync_from_wall || self.live_head_ms == 0 {
            self.live_head_ms = now_timestamp_ms().saturating_sub(duration_ms);
            self.resync_from_wall = false;
        }

        self.live_head_ms = self.live_head_ms.saturating_add(duration_ms);
        Some(self.live_head_ms)
    }

    fn flush_if_due(&mut self, clock: &Arc<PlaybackClock>) {
        if self.live_head_ms == 0 {
            return;
        }

        if self.last_flushed_ms == 0
            || self.live_head_ms.saturating_sub(self.last_flushed_ms) >= LIVE_CLOCK_FLUSH_MS
        {
            self.flush_now(clock);
        }
    }

    fn flush_now(&mut self, clock: &Arc<PlaybackClock>) {
        if self.live_head_ms == 0 {
            return;
        }

        clock.set_live_head_ms(self.live_head_ms);
        clock.set_playback_cursor_ms(self.live_head_ms);
        self.last_flushed_ms = self.live_head_ms;
    }
}

fn read_prefetched_chunk(
    store: &Arc<Mutex<TimeshiftStore>>,
    cursor: &PlaybackReadCursor,
    expected_channels: u16,
    expected_sample_rate: u32,
    fft_state: &mut FftVizState,
) -> Result<PrefetchChunkResult> {
    let mut next_cursor = cursor.clone();
    let store = store.lock().expect("timeshift mutex poisoned");
    let Some(audio) = store.read_chunk_from(&mut next_cursor, OUTPUT_CHUNK_MS)? else {
        return Ok(PrefetchChunkResult::Pending);
    };
    drop(store);

    if audio.channels != expected_channels || audio.sample_rate != expected_sample_rate {
        return Ok(PrefetchChunkResult::RestartRequired);
    }

    Ok(PrefetchChunkResult::Ready(
        QueuedPlaybackChunk::from_audio(audio, fft_state, playback_viz_params()),
        next_cursor,
    ))
}

fn run_live_playback_prefetch(
    live_buffer: Arc<LiveChunkBuffer>,
    queue: Arc<PlaybackQueue>,
    event_tx: mpsc::Sender<PlaybackEvent>,
    stop_requested: Arc<AtomicBool>,
    mut cursor: LiveReadCursor,
    expected_channels: u16,
    expected_sample_rate: u32,
    mut fft_state: FftVizState,
) {
    let mut wait_generation = 0;

    while !stop_requested.load(Ordering::Relaxed) {
        let Some(audio) = live_buffer.read_chunk_from(&mut cursor, OUTPUT_CHUNK_MS) else {
            live_buffer.wait_for_change(&mut wait_generation, &stop_requested);
            continue;
        };

        if audio.channels != expected_channels || audio.sample_rate != expected_sample_rate {
            let _ = event_tx.send(PlaybackEvent::RestartRequired);
            queue.close(PlaybackQueueCloseReason::RestartRequired);
            return;
        }

        let chunk = QueuedPlaybackChunk::from_audio(audio, &mut fft_state, playback_viz_params());
        if !queue.push_blocking(chunk, &stop_requested) {
            queue.close(PlaybackQueueCloseReason::Stopped);
            return;
        }
    }

    queue.close(PlaybackQueueCloseReason::Stopped);
}

fn run_playback_prefetch(
    store: Arc<Mutex<TimeshiftStore>>,
    notifier: Arc<PlaybackNotifier>,
    queue: Arc<PlaybackQueue>,
    event_tx: mpsc::Sender<PlaybackEvent>,
    stop_requested: Arc<AtomicBool>,
    mut cursor: PlaybackReadCursor,
    expected_channels: u16,
    expected_sample_rate: u32,
    mut fft_state: FftVizState,
) {
    let mut wait_generation = 0;

    while !stop_requested.load(Ordering::Relaxed) {
        match read_prefetched_chunk(
            &store,
            &cursor,
            expected_channels,
            expected_sample_rate,
            &mut fft_state,
        ) {
            Ok(PrefetchChunkResult::Ready(chunk, next_cursor)) => {
                cursor = next_cursor;
                if !queue.push_blocking(chunk, &stop_requested) {
                    break;
                }
            }
            Ok(PrefetchChunkResult::Pending) => {
                notifier.wait_for_change(&mut wait_generation, &stop_requested);
            }
            Ok(PrefetchChunkResult::RestartRequired) => {
                let _ = event_tx.send(PlaybackEvent::RestartRequired);
                queue.close(PlaybackQueueCloseReason::RestartRequired);
                return;
            }
            Err(err) => {
                eprintln!("playback prefetch error: {err}");
                stop_requested.store(true, Ordering::Relaxed);
                break;
            }
        }
    }

    queue.close(PlaybackQueueCloseReason::Stopped);
}

fn try_start_live_playback(
    live_buffer: &Arc<LiveChunkBuffer>,
    cursor_ms: u64,
    clock: Arc<PlaybackClock>,
    stop_requested: Arc<AtomicBool>,
    output: Arc<queue::SourcesQueueInput>,
    playback_generation: Arc<AtomicU64>,
    expected_generation: u64,
    spectrum_bits: Arc<Vec<AtomicU32>>,
    event_tx: mpsc::Sender<PlaybackEvent>,
) -> Option<PlaybackStart> {
    let mut cursor = live_buffer.cursor_for_ms(cursor_ms)?;
    let initial_audio = live_buffer.read_chunk_from(&mut cursor, OUTPUT_CHUNK_MS)?;

    let mut fft_state = make_fft_state(spectrum_bits.len());
    let expected_channels = initial_audio.channels;
    let expected_sample_rate = initial_audio.sample_rate;
    let initial_chunk =
        QueuedPlaybackChunk::from_audio(initial_audio, &mut fft_state, playback_viz_params());

    let queue = Arc::new(PlaybackQueue::default());
    queue.push_prefilled(initial_chunk);
    for _ in 0..PLAYBACK_PREFILL_CHUNKS.saturating_sub(1) {
        let Some(audio) = live_buffer.read_chunk_from(&mut cursor, OUTPUT_CHUNK_MS) else {
            break;
        };
        queue.push_prefilled(QueuedPlaybackChunk::from_audio(
            audio,
            &mut fft_state,
            playback_viz_params(),
        ));
    }

    let prefetch_stop_requested = stop_requested.clone();
    let prefetch_handle = {
        let live_buffer = live_buffer.clone();
        let queue = queue.clone();
        let event_tx = event_tx.clone();
        thread::spawn(move || {
            run_live_playback_prefetch(
                live_buffer,
                queue,
                event_tx,
                prefetch_stop_requested,
                cursor,
                expected_channels,
                expected_sample_rate,
                fft_state,
            )
        })
    };
    let output_handle = {
        let queue = queue.clone();
        let stop_requested = stop_requested.clone();
        let clock = clock.clone();
        let spectrum_bits = spectrum_bits.clone();
        let playback_generation = playback_generation.clone();
        thread::spawn(move || {
            run_playback_output(
                output,
                queue,
                stop_requested,
                clock,
                spectrum_bits,
                playback_generation,
                expected_generation,
            )
        })
    };

    Some(PlaybackStart {
        queue,
        prefetch_handle,
        output_handle,
    })
}

fn try_start_playback(
    live_buffer: &Arc<LiveChunkBuffer>,
    clock: Arc<PlaybackClock>,
    stop_requested: Arc<AtomicBool>,
    output: Arc<queue::SourcesQueueInput>,
    playback_generation: Arc<AtomicU64>,
    expected_generation: u64,
    spectrum_bits: Arc<Vec<AtomicU32>>,
    event_tx: mpsc::Sender<PlaybackEvent>,
) -> Result<Option<PlaybackStart>> {
    let live_head = clock.live_head_ms();
    let floor_ms = live_buffer.earliest_ms();
    if live_head == 0 {
        return Ok(None);
    }

    let requested_cursor_ms = match floor_ms {
        Some(floor_ms) => clock.playback_cursor_ms().max(floor_ms).min(live_head),
        None => clock.playback_cursor_ms().min(live_head),
    };
    let cursor_ms = if requested_cursor_ms == 0 {
        buffered_playback_start_ms(live_head, floor_ms)
    } else {
        requested_cursor_ms
    };

    if live_head.saturating_sub(cursor_ms) < OUTPUT_MIN_HEADROOM_MS {
        return Ok(None);
    }

    if cursor_ms != clock.playback_cursor_ms() {
        clock.set_playback_cursor_ms(cursor_ms);
    }

    Ok(try_start_live_playback(
        live_buffer,
        cursor_ms,
        clock,
        stop_requested,
        output,
        playback_generation,
        expected_generation,
        spectrum_bits,
        event_tx,
    ))
}

fn teardown_playback_pipeline(
    output: Option<&Arc<queue::SourcesQueueInput>>,
    playback_generation: &Arc<AtomicU64>,
    live_buffer: &Arc<LiveChunkBuffer>,
    notifier: &Arc<PlaybackNotifier>,
    playback_queue: &mut Option<Arc<PlaybackQueue>>,
    prefetch_worker: &mut Option<thread::JoinHandle<()>>,
    output_worker: &mut Option<thread::JoinHandle<()>>,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
) {
    playback_generation.fetch_add(1, Ordering::Relaxed);
    if let Some(output) = output {
        output.clear();
    }
    if let Some(queue) = playback_queue.take() {
        queue.close(PlaybackQueueCloseReason::Stopped);
    }
    live_buffer.wake();
    notifier.notify();
    if let Some(handle) = prefetch_worker.take() {
        let _ = handle.join();
    }
    if let Some(handle) = output_worker.take() {
        let _ = handle.join();
    }
    clear_spectrum(spectrum_bits);
}

fn run_buffer_relay(
    live_buffer: Arc<LiveChunkBuffer>,
    clock: Arc<PlaybackClock>,
    direct_live_enabled: Arc<AtomicBool>,
    stop_requested: Arc<AtomicBool>,
    rx: mpsc::Receiver<StoreCommand>,
) -> Result<()> {
    let mut live_head_ms = 0;

    while let Ok(cmd) = rx.recv() {
        match cmd {
            StoreCommand::Append {
                sample_rate,
                channels,
                samples,
                now_ms,
            } => {
                let Some((start_ms, end_ms)) = compute_chunk_timing(
                    live_head_ms,
                    sample_rate,
                    channels,
                    samples.len(),
                    now_ms,
                ) else {
                    continue;
                };

                live_head_ms = end_ms;
                clock.set_live_head_ms(end_ms);
                if direct_live_enabled.load(Ordering::Relaxed) {
                    clock.set_playback_cursor_ms(end_ms);
                }
                live_buffer.push(StoredPcmChunk {
                    channels,
                    sample_rate,
                    samples: samples.as_ref().to_vec(),
                    start_ms,
                    end_ms,
                });

                if !direct_live_enabled.load(Ordering::Relaxed) && clock.playback_cursor_ms() == 0 {
                    clock.set_playback_cursor_ms(buffered_playback_start_ms(
                        end_ms,
                        live_buffer.earliest_ms(),
                    ));
                }
            }
        }

        if stop_requested.load(Ordering::Relaxed) {
            break;
        }
    }

    Ok(())
}

fn run_store_writer(
    store: Arc<Mutex<TimeshiftStore>>,
    clock: Arc<PlaybackClock>,
    notifier: Arc<PlaybackNotifier>,
    stop_requested: Arc<AtomicBool>,
    rx: mpsc::Receiver<StoreCommand>,
) -> Result<()> {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            StoreCommand::Append {
                sample_rate,
                channels,
                samples,
                now_ms,
            } => {
                let mut store = store.lock().expect("timeshift mutex poisoned");
                let (_start_ms, end_ms) =
                    store.append_pcm(sample_rate, channels, samples.as_ref(), now_ms)?;
                let floor = store.earliest_timestamp_ms();
                drop(store);

                if clock.playback_cursor_ms() != 0 {
                    if let Some(floor) = floor {
                        clock.clamp_playback_floor(floor);
                    }
                } else {
                    clock.set_playback_cursor_ms(buffered_playback_start_ms(end_ms, floor));
                }

                notifier.notify();
            }
        }

        if stop_requested.load(Ordering::Relaxed) {
            break;
        }
    }

    Ok(())
}

fn run_one_connection(
    format: &mut Box<dyn symphonia::core::formats::FormatReader>,
    track_id: &mut u32,
    decoder: &mut Box<dyn symphonia::core::codecs::Decoder>,
    decoder_opts: &DecoderOptions,
    clock: &Arc<PlaybackClock>,
    stop_requested: &Arc<AtomicBool>,
    buffer_tx: &mpsc::Sender<StoreCommand>,
    live_cmd_rx: &mpsc::Receiver<LivePlayerCommand>,
    direct_player: &mut Player,
    direct_mixer: &rodio::mixer::Mixer,
    direct_live_enabled: &Arc<AtomicBool>,
    capture_enabled: &Arc<AtomicBool>,
    fft_state: &mut FftVizState,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
) -> Result<RunOutcome> {
    let mut decode_state = DecodeState {
        sample_buf: None,
        channels: 0,
        sample_rate: 0,
    };

    loop {
        if stop_requested.load(Ordering::Relaxed) {
            return Ok(RunOutcome::Stop);
        }

        while let Ok(cmd) = live_cmd_rx.try_recv() {
            match cmd {
                LivePlayerCommand::Reset => reset_direct_player(direct_player, direct_mixer),
                LivePlayerCommand::Stop => {
                    direct_player.stop();
                    return Ok(RunOutcome::Stop);
                }
            }
        }

        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::ResetRequired) => {
                if is_verbose() {
                    println!("[{}] Stream reset, reconfiguring decoder…", now_string());
                }

                let new_track = format
                    .tracks()
                    .iter()
                    .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
                    .ok_or_else(|| "no supported audio tracks after reset".to_string())?;

                *track_id = new_track.id;
                *decoder =
                    symphonia::default::get_codecs().make(&new_track.codec_params, decoder_opts)?;
                decode_state.sample_buf = None;
                if direct_live_enabled.load(Ordering::Relaxed)
                    && !capture_enabled.load(Ordering::Relaxed)
                {
                    reset_direct_player(direct_player, direct_mixer);
                    reset_fft_state(
                        &mut fft_state.mono_ring,
                        &mut fft_state.bars_smooth,
                        &mut fft_state.bar_peak,
                        spectrum_bits,
                    );
                }
                continue;
            }
            Err(err) => {
                if direct_live_enabled.load(Ordering::Relaxed)
                    && !capture_enabled.load(Ordering::Relaxed)
                {
                    reset_direct_player(direct_player, direct_mixer);
                }
                eprintln!("Error reading packet: {err:?}");
                return Ok(RunOutcome::Reconnect);
            }
        };

        let live_mode =
            direct_live_enabled.load(Ordering::Relaxed) && !capture_enabled.load(Ordering::Relaxed);

        let (outcome, audio) = decode_and_process_packet(
            &packet,
            format,
            track_id,
            decoder,
            decoder_opts,
            live_mode,
            spectrum_bits,
            &mut decode_state,
            fft_state,
            if live_mode {
                live_viz_params()
            } else {
                playback_viz_params()
            },
        )?;

        match outcome {
            PacketOutcome::Continue => {}
            PacketOutcome::Reconnect => {
                if live_mode {
                    reset_direct_player(direct_player, direct_mixer);
                }
                return Ok(RunOutcome::Reconnect);
            }
            PacketOutcome::SpecChanged => {
                if live_mode {
                    reset_direct_player(direct_player, direct_mixer);
                    reset_fft_state(
                        &mut fft_state.mono_ring,
                        &mut fft_state.bars_smooth,
                        &mut fft_state.bar_peak,
                        spectrum_bits,
                    );
                }
                continue;
            }
        }

        if let Some((channels, sample_rate, samples)) = audio {
            let now_ms = now_timestamp_ms();

            if live_mode {
                append_samples_in_chunks(direct_player, channels, sample_rate, &samples);
                if let Some((_start_ms, end_ms)) = compute_chunk_timing(
                    clock.live_head_ms(),
                    sample_rate,
                    channels,
                    samples.len(),
                    now_ms,
                ) {
                    clock.set_live_head_ms(end_ms);
                    clock.set_playback_cursor_ms(end_ms);
                }
            }

            if capture_enabled.load(Ordering::Relaxed) {
                let samples: Arc<[f32]> = samples.into();
                if buffer_tx
                    .send(StoreCommand::Append {
                        sample_rate,
                        channels,
                        samples,
                        now_ms,
                    })
                    .is_err()
                {
                    return Ok(RunOutcome::Stop);
                }
            }
        }
    }
}

fn run_ingest_loop(
    station: Station,
    clock: Arc<PlaybackClock>,
    stop_requested: Arc<AtomicBool>,
    buffer_tx: mpsc::Sender<StoreCommand>,
    live_cmd_rx: mpsc::Receiver<LivePlayerCommand>,
    direct_mixer: rodio::mixer::Mixer,
    direct_live_enabled: Arc<AtomicBool>,
    capture_enabled: Arc<AtomicBool>,
    spectrum_bits: Arc<Vec<AtomicU32>>,
) -> Result<()> {
    let primary = station.stream_url().to_string();
    let fallback = station.stream_fallback_url().to_string();
    let mut use_fallback = false;

    let mut client = build_client()?;
    let useragent = build_useragent();

    let format_opts: FormatOptions = Default::default();
    let metadata_opts: MetadataOptions = Default::default();
    let decoder_opts: DecoderOptions = Default::default();
    let mut fft_state = make_fft_state(spectrum_bits.len());
    let mut direct_player = Player::connect_new(&direct_mixer);

    while !stop_requested.load(Ordering::Relaxed) {
        let url: &str = if use_fallback { &fallback } else { &primary };
        let (mut format, mut track_id, mut decoder) = match open_stream(
            url,
            &client,
            &useragent,
            &format_opts,
            &metadata_opts,
            &decoder_opts,
        ) {
            Ok(parts) => parts,
            Err(err) => {
                eprintln!("connect/probe error on {url}: {err}");
                if !fallback.is_empty() {
                    use_fallback = !use_fallback;
                }
                client = build_client()?;
                continue;
            }
        };

        if is_verbose() {
            println!("[{}] Started buffering live stream.", now_string());
        }

        if direct_live_enabled.load(Ordering::Relaxed) {
            reset_direct_player(&mut direct_player, &direct_mixer);
        }

        match run_one_connection(
            &mut format,
            &mut track_id,
            &mut decoder,
            &decoder_opts,
            &clock,
            &stop_requested,
            &buffer_tx,
            &live_cmd_rx,
            &mut direct_player,
            &direct_mixer,
            &direct_live_enabled,
            &capture_enabled,
            &mut fft_state,
            &spectrum_bits,
        )? {
            RunOutcome::Stop => return Ok(()),
            RunOutcome::Reconnect => {
                if !fallback.is_empty() {
                    use_fallback = !use_fallback;
                }
                continue;
            }
        }
    }

    Ok(())
}

pub(super) fn run_listenmoe_stream(
    station: Station,
    rx: mpsc::Receiver<Control>,
    spectrum_bits: Arc<Vec<AtomicU32>>,
    clock: Arc<PlaybackClock>,
    root: PathBuf,
) -> Result<()> {
    clock.reset();
    let mut stream = DeviceSinkBuilder::open_default_sink()?;
    stream.log_on_drop(false);

    match run_direct_live_until_pause(station, &rx, &spectrum_bits, &clock, stream.mixer())? {
        LiveDirectOutcome::Stop => {
            clear_root(&root)?;
            return Ok(());
        }
        LiveDirectOutcome::TransitionToBuffered => {}
    }

    let live_buffer = Arc::new(LiveChunkBuffer::default());
    let notifier = Arc::new(PlaybackNotifier::default());
    let stop_requested = Arc::new(AtomicBool::new(false));
    let direct_live_enabled = Arc::new(AtomicBool::new(false));
    let capture_enabled = Arc::new(AtomicBool::new(true));
    let mut output = None;
    let (_live_cmd_tx, live_cmd_rx) = mpsc::channel();
    let (buffer_tx, buffer_rx) = mpsc::channel();
    let buffer_handle = {
        let live_buffer = live_buffer.clone();
        let clock = clock.clone();
        let direct_live_enabled = direct_live_enabled.clone();
        let stop_requested = stop_requested.clone();
        thread::spawn(move || {
            let buffer_stop_requested = stop_requested.clone();
            if let Err(err) = run_buffer_relay(
                live_buffer,
                clock,
                direct_live_enabled,
                buffer_stop_requested,
                buffer_rx,
            ) {
                eprintln!("buffer relay error: {err}");
                stop_requested.store(true, Ordering::Relaxed);
            }
        })
    };
    let ingest_handle = {
        let clock = clock.clone();
        let stop_requested = stop_requested.clone();
        let buffer_tx = buffer_tx.clone();
        let direct_mixer = stream.mixer().clone();
        let direct_live_enabled = direct_live_enabled.clone();
        let capture_enabled = capture_enabled.clone();
        let spectrum_bits = spectrum_bits.clone();
        thread::spawn(move || {
            run_ingest_loop(
                station,
                clock,
                stop_requested,
                buffer_tx,
                live_cmd_rx,
                direct_mixer,
                direct_live_enabled,
                capture_enabled,
                spectrum_bits,
            )
        })
    };
    let playback_generation = Arc::new(AtomicU64::new(1));
    let (playback_event_tx, playback_event_rx) = mpsc::channel();
    let mut playback_queue = None;
    let mut playback_prefetch_worker = None;
    let mut playback_output_worker = None;
    let mut paused = true;
    let mut source_started = false;

    loop {
        match handle_output_control(&rx, &mut paused, &stop_requested, &spectrum_bits)? {
            OutputCommandResult::Continue => {}
            OutputCommandResult::Stop => {
                stop_requested.store(true, Ordering::Relaxed);
                break;
            }
            OutputCommandResult::RestartPlayback => {
                source_started = false;
                teardown_playback_pipeline(
                    output.as_ref(),
                    &playback_generation,
                    &live_buffer,
                    &notifier,
                    &mut playback_queue,
                    &mut playback_prefetch_worker,
                    &mut playback_output_worker,
                    &spectrum_bits,
                );
            }
        }
        if stop_requested.load(Ordering::Relaxed) {
            break;
        }

        while let Ok(event) = playback_event_rx.try_recv() {
            match event {
                PlaybackEvent::RestartRequired => {
                    source_started = false;
                    teardown_playback_pipeline(
                        output.as_ref(),
                        &playback_generation,
                        &live_buffer,
                        &notifier,
                        &mut playback_queue,
                        &mut playback_prefetch_worker,
                        &mut playback_output_worker,
                        &spectrum_bits,
                    );
                }
            }
        }

        if !paused && !source_started && !direct_live_enabled.load(Ordering::Relaxed) {
            if output.is_none() {
                let (new_output, output_source) = queue::queue(true);
                stream.mixer().add(output_source);
                output = Some(new_output);
            }

            if let Some(playback) = try_start_playback(
                &live_buffer,
                clock.clone(),
                stop_requested.clone(),
                output
                    .as_ref()
                    .expect("playback output must exist when starting playback")
                    .clone(),
                playback_generation.clone(),
                playback_generation.load(Ordering::Relaxed),
                spectrum_bits.clone(),
                playback_event_tx.clone(),
            )? {
                let PlaybackStart {
                    queue,
                    prefetch_handle,
                    output_handle,
                } = playback;
                playback_queue = Some(queue);
                playback_prefetch_worker = Some(prefetch_handle);
                playback_output_worker = Some(output_handle);
                source_started = true;
                continue;
            }
        }

        match rx.recv_timeout(Duration::from_millis(OUTPUT_WAIT_TIMEOUT_MS)) {
            Ok(cmd) => {
                match handle_output_command(cmd, &mut paused, &stop_requested, &spectrum_bits)? {
                    OutputCommandResult::Continue => {}
                    OutputCommandResult::Stop => {
                        stop_requested.store(true, Ordering::Relaxed);
                        break;
                    }
                    OutputCommandResult::RestartPlayback => {
                        source_started = false;
                        teardown_playback_pipeline(
                            output.as_ref(),
                            &playback_generation,
                            &live_buffer,
                            &notifier,
                            &mut playback_queue,
                            &mut playback_prefetch_worker,
                            &mut playback_output_worker,
                            &spectrum_bits,
                        );
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    stop_requested.store(true, Ordering::Relaxed);
    drop(buffer_tx);
    teardown_playback_pipeline(
        output.as_ref(),
        &playback_generation,
        &live_buffer,
        &notifier,
        &mut playback_queue,
        &mut playback_prefetch_worker,
        &mut playback_output_worker,
        &spectrum_bits,
    );
    let _ = ingest_handle.join();
    let _ = buffer_handle.join();
    clear_root(&root)?;

    Ok(())
}

fn now_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::{buffered_playback_start_ms, LiveChunkBuffer};
    use crate::listen::store::StoredPcmChunk;

    #[test]
    fn startup_cursor_keeps_configured_headroom() {
        assert_eq!(buffered_playback_start_ms(10_000, None), 6_000);
    }

    #[test]
    fn startup_cursor_respects_retention_floor() {
        assert_eq!(buffered_playback_start_ms(10_000, Some(9_500)), 9_500);
    }

    #[test]
    fn live_buffer_sequential_reads_do_not_drop_or_duplicate_samples() {
        let live_buffer = LiveChunkBuffer::default();
        let original: Vec<f32> = (0..44_100).map(|value| value as f32 / 16.0).collect();

        for (index, chunk) in original.chunks(997).enumerate() {
            let frames_before = index * 997;
            let frames_after = frames_before + chunk.len();
            live_buffer.push(StoredPcmChunk {
                channels: 1,
                sample_rate: 44_100,
                samples: chunk.to_vec(),
                start_ms: ((frames_before as u64) * 1_000) / 44_100,
                end_ms: ((frames_after as u64) * 1_000) / 44_100,
            });
        }

        let mut cursor = live_buffer.cursor_for_ms(0).expect("cursor");
        let mut recovered = Vec::new();

        while let Some(chunk) = live_buffer.read_chunk_from(&mut cursor, 137) {
            recovered.extend_from_slice(&chunk.samples);
            if recovered.len() >= original.len() {
                break;
            }
        }

        assert_eq!(recovered, original);
    }

    #[test]
    fn live_buffer_reads_merge_adjacent_audio_up_to_target_duration() {
        let live_buffer = LiveChunkBuffer::default();
        for (start_ms, value) in [(0_u64, 0.0_f32), (100_u64, 1.0_f32), (200_u64, 2.0_f32)] {
            live_buffer.push(StoredPcmChunk {
                channels: 2,
                sample_rate: 1_000,
                samples: vec![value; 400],
                start_ms,
                end_ms: start_ms + 100,
            });
        }

        let mut cursor = live_buffer.cursor_for_ms(0).expect("cursor");
        let merged = live_buffer
            .read_chunk_from(&mut cursor, 1_000)
            .expect("merged chunk");

        assert_eq!(merged.start_ms, 0);
        assert_eq!(merged.end_ms, 300);
        assert_eq!(merged.samples.len(), 1_200);
    }
}
