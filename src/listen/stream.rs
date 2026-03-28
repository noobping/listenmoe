use reqwest::blocking::Client;
use rodio::{DeviceSinkBuilder, Player, Source};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
use super::store::{clear_root, PlaybackReadCursor, StoredPcmChunk, TimeshiftStore, RETENTION_MS};
use super::viz::{
    apply_spectrum_snapshot, build_spectrum_snapshot, clear_spectrum, decode_and_process_packet,
    make_fft_state, DecodeState, FftVizState, PacketOutcome, VizParams,
};
use super::{Control, Result};

#[derive(Debug, Clone, Copy)]
enum RunOutcome {
    Stop,
    Reconnect,
}

enum OutputCommandResult {
    Continue,
    Stop,
    RestartPlayback,
}

const OUTPUT_CHUNK_MS: u64 = 1_000;
const OUTPUT_MIN_HEADROOM_MS: u64 = 4_000;
const OUTPUT_WAIT_TIMEOUT_MS: u64 = 25;
const LIVE_BUFFER_MS: u64 = 10 * 60 * 1_000;
const PLAYBACK_PREFILL_CHUNKS: usize = 6;
const PLAYBACK_QUEUE_MAX_CHUNKS: usize = 8;

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
    chunks: VecDeque<StoredPcmChunk>,
    generation: u64,
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
        state.chunks.push_back(chunk);

        let prune_before = live_head_ms.saturating_sub(LIVE_BUFFER_MS);
        while state.chunks.len() > 1 {
            let should_remove = state
                .chunks
                .front()
                .map(|chunk| chunk.end_ms <= prune_before)
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
            .map(|chunk| chunk.start_ms)
    }

    fn snapshot_from(&self, cursor_ms: u64) -> (Vec<StoredPcmChunk>, u64) {
        let state = self.state.lock().expect("live chunk buffer poisoned");
        let chunks = state
            .chunks
            .iter()
            .filter_map(|chunk| slice_chunk_from_ms(chunk, cursor_ms))
            .collect();
        (chunks, state.generation)
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
}

struct TimeshiftPlaybackSource {
    queue: Arc<PlaybackQueue>,
    clock: Arc<PlaybackClock>,
    stop_requested: Arc<AtomicBool>,
    spectrum_bits: Arc<Vec<AtomicU32>>,
    current_chunk: QueuedPlaybackChunk,
    current_index: usize,
    chunk_announced: bool,
}

impl TimeshiftPlaybackSource {
    fn new(
        queue: Arc<PlaybackQueue>,
        clock: Arc<PlaybackClock>,
        stop_requested: Arc<AtomicBool>,
        spectrum_bits: Arc<Vec<AtomicU32>>,
        current_chunk: QueuedPlaybackChunk,
    ) -> Self {
        Self {
            queue,
            clock,
            stop_requested,
            spectrum_bits,
            current_chunk,
            current_index: 0,
            chunk_announced: false,
        }
    }

    fn remaining_samples(&self) -> usize {
        self.current_chunk
            .audio
            .samples
            .len()
            .saturating_sub(self.current_index)
    }

    fn ensure_chunk_announced(&mut self) -> Option<()> {
        if self.chunk_announced {
            return Some(());
        }

        apply_spectrum_snapshot(&self.spectrum_bits, &self.current_chunk.spectrum_snapshot);
        self.chunk_announced = true;
        Some(())
    }

    fn advance_chunk(&mut self) -> Option<()> {
        self.clock
            .set_playback_cursor_ms(self.current_chunk.audio.end_ms);

        if let Some(next_chunk) = self.queue.pop_blocking(&self.stop_requested) {
            self.current_chunk = next_chunk;
            self.current_index = 0;
            self.chunk_announced = false;
            return Some(());
        }

        clear_spectrum(&self.spectrum_bits);

        None
    }
}

impl Iterator for TimeshiftPlaybackSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_index >= self.current_chunk.audio.samples.len() {
            self.advance_chunk()?;
        }

        self.ensure_chunk_announced()?;

        let sample = self
            .current_chunk
            .audio
            .samples
            .get(self.current_index)
            .copied()?;
        self.current_index += 1;

        Some(sample)
    }
}

impl Source for TimeshiftPlaybackSource {
    fn current_span_len(&self) -> Option<usize> {
        match self.remaining_samples() {
            0 => None,
            remaining => Some(remaining),
        }
    }

    fn channels(&self) -> rodio::ChannelCount {
        rodio::ChannelCount::new(self.current_chunk.audio.channels)
            .expect("timeshift chunk must have non-zero channels")
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        rodio::SampleRate::new(self.current_chunk.audio.sample_rate)
            .expect("timeshift chunk must have non-zero sample rate")
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

fn slice_chunk_from_ms(chunk: &StoredPcmChunk, cursor_ms: u64) -> Option<StoredPcmChunk> {
    if chunk.end_ms <= cursor_ms {
        return None;
    }

    if cursor_ms <= chunk.start_ms {
        return Some(chunk.clone());
    }

    let channels = usize::from(chunk.channels);
    if channels == 0 || chunk.sample_rate == 0 {
        return None;
    }

    let total_frames = chunk.samples.len() / channels;
    if total_frames == 0 {
        return None;
    }

    let frame_offset =
        (((cursor_ms - chunk.start_ms).saturating_mul(u64::from(chunk.sample_rate))) / 1_000)
            .min(total_frames as u64);
    if frame_offset >= total_frames as u64 {
        return None;
    }

    let start_sample = frame_offset as usize * channels;
    let start_ms = chunk
        .start_ms
        .saturating_add((frame_offset.saturating_mul(1_000)) / u64::from(chunk.sample_rate));

    Some(StoredPcmChunk {
        channels: chunk.channels,
        sample_rate: chunk.sample_rate,
        samples: chunk.samples[start_sample..].to_vec(),
        start_ms,
        end_ms: chunk.end_ms,
    })
}

fn coalesce_playback_chunks(
    chunks: Vec<StoredPcmChunk>,
    target_duration_ms: u64,
) -> Vec<StoredPcmChunk> {
    let mut merged = Vec::new();

    for chunk in chunks {
        match merged.last_mut() {
            Some(current)
                if current.channels == chunk.channels
                    && current.sample_rate == chunk.sample_rate
                    && current.end_ms == chunk.start_ms
                    && current.end_ms.saturating_sub(current.start_ms) < target_duration_ms =>
            {
                current.samples.extend_from_slice(&chunk.samples);
                current.end_ms = chunk.end_ms;
            }
            _ => merged.push(chunk),
        }
    }

    merged
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

fn handle_output_command(
    cmd: Control,
    sink: &Player,
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
            sink.stop();
            clear_spectrum(spectrum_bits);
            return Ok(OutputCommandResult::Stop);
        }
        Control::Pause => {
            if !*paused {
                if is_verbose() {
                    println!("[{}] Pausing playback.", now_string());
                }
                *paused = true;
                sink.stop();
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
                sink.play();
                return Ok(OutputCommandResult::RestartPlayback);
            }
        }
    }

    Ok(OutputCommandResult::Continue)
}

fn handle_output_control(
    rx: &mpsc::Receiver<Control>,
    sink: &Player,
    paused: &mut bool,
    stop_requested: &Arc<AtomicBool>,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
) -> Result<OutputCommandResult> {
    while let Ok(cmd) = rx.try_recv() {
        match handle_output_command(cmd, sink, paused, stop_requested, spectrum_bits)? {
            OutputCommandResult::Continue => {}
            result => return Ok(result),
        }
    }

    Ok(OutputCommandResult::Continue)
}

struct PlaybackStart {
    queue: Arc<PlaybackQueue>,
    source: TimeshiftPlaybackSource,
    worker_handle: thread::JoinHandle<()>,
}

enum StoreCommand {
    Append {
        sample_rate: u32,
        channels: u16,
        samples: Vec<f32>,
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
    mut cursor_ms: u64,
    expected_channels: u16,
    expected_sample_rate: u32,
    mut fft_state: FftVizState,
) {
    let mut wait_generation = 0;

    while !stop_requested.load(Ordering::Relaxed) {
        let (chunks, generation) = live_buffer.snapshot_from(cursor_ms);
        wait_generation = generation;
        let chunks = coalesce_playback_chunks(chunks, OUTPUT_CHUNK_MS);

        if chunks.is_empty() {
            live_buffer.wait_for_change(&mut wait_generation, &stop_requested);
            continue;
        }

        for audio in chunks {
            if audio.channels != expected_channels || audio.sample_rate != expected_sample_rate {
                let _ = event_tx.send(PlaybackEvent::RestartRequired);
                queue.close(PlaybackQueueCloseReason::RestartRequired);
                return;
            }

            cursor_ms = audio.end_ms;
            let chunk =
                QueuedPlaybackChunk::from_audio(audio, &mut fft_state, playback_viz_params());
            if !queue.push_blocking(chunk, &stop_requested) {
                queue.close(PlaybackQueueCloseReason::Stopped);
                return;
            }
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
    spectrum_bits: Arc<Vec<AtomicU32>>,
    event_tx: mpsc::Sender<PlaybackEvent>,
) -> Option<PlaybackStart> {
    let (chunks, _) = live_buffer.snapshot_from(cursor_ms);
    let mut chunks = coalesce_playback_chunks(chunks, OUTPUT_CHUNK_MS).into_iter();
    let initial_audio = chunks.next()?;

    let mut fft_state = make_fft_state(spectrum_bits.len());
    let expected_channels = initial_audio.channels;
    let expected_sample_rate = initial_audio.sample_rate;
    let mut worker_cursor_ms = initial_audio.end_ms;
    let initial_chunk =
        QueuedPlaybackChunk::from_audio(initial_audio, &mut fft_state, playback_viz_params());

    let queue = Arc::new(PlaybackQueue::default());
    for audio in chunks.take(PLAYBACK_PREFILL_CHUNKS.saturating_sub(1)) {
        worker_cursor_ms = audio.end_ms;
        queue.push_prefilled(QueuedPlaybackChunk::from_audio(
            audio,
            &mut fft_state,
            playback_viz_params(),
        ));
    }

    let source = TimeshiftPlaybackSource::new(
        queue.clone(),
        clock,
        stop_requested.clone(),
        spectrum_bits,
        initial_chunk,
    );
    let worker_handle = {
        let live_buffer = live_buffer.clone();
        let queue = queue.clone();
        let event_tx = event_tx.clone();
        thread::spawn(move || {
            run_live_playback_prefetch(
                live_buffer,
                queue,
                event_tx,
                stop_requested,
                worker_cursor_ms,
                expected_channels,
                expected_sample_rate,
                fft_state,
            )
        })
    };

    Some(PlaybackStart {
        queue,
        source,
        worker_handle,
    })
}

fn try_start_playback(
    store: &Arc<Mutex<TimeshiftStore>>,
    live_buffer: &Arc<LiveChunkBuffer>,
    notifier: Arc<PlaybackNotifier>,
    clock: Arc<PlaybackClock>,
    stop_requested: Arc<AtomicBool>,
    spectrum_bits: Arc<Vec<AtomicU32>>,
    event_tx: mpsc::Sender<PlaybackEvent>,
) -> Result<Option<PlaybackStart>> {
    let mut guard = store.lock().expect("timeshift mutex poisoned");
    let live_head = guard.live_head_ms();
    let floor_ms = guard.earliest_timestamp_ms();
    if live_head == 0 {
        return Ok(None);
    }

    let requested_cursor_ms = guard.clamp_cursor_ms(clock.playback_cursor_ms());
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

    drop(guard);

    if live_buffer
        .earliest_ms()
        .is_some_and(|earliest_ms| cursor_ms >= earliest_ms)
    {
        if let Some(playback) = try_start_live_playback(
            live_buffer,
            cursor_ms,
            clock.clone(),
            stop_requested.clone(),
            spectrum_bits.clone(),
            event_tx.clone(),
        ) {
            return Ok(Some(playback));
        }
    }

    let mut guard = store.lock().expect("timeshift mutex poisoned");
    let Some(mut cursor) = guard.cursor_for_ms(cursor_ms) else {
        return Ok(None);
    };
    let Some(initial_audio) = guard.read_chunk_from(&mut cursor, OUTPUT_CHUNK_MS)? else {
        return Ok(None);
    };
    drop(guard);

    let mut fft_state = make_fft_state(spectrum_bits.len());
    let expected_channels = initial_audio.channels;
    let expected_sample_rate = initial_audio.sample_rate;
    let initial_chunk =
        QueuedPlaybackChunk::from_audio(initial_audio, &mut fft_state, playback_viz_params());

    let queue = Arc::new(PlaybackQueue::default());
    for _ in 0..PLAYBACK_PREFILL_CHUNKS {
        match read_prefetched_chunk(
            store,
            &cursor,
            expected_channels,
            expected_sample_rate,
            &mut fft_state,
        )? {
            PrefetchChunkResult::Ready(chunk, next_cursor) => {
                cursor = next_cursor;
                queue.push_prefilled(chunk);
            }
            PrefetchChunkResult::Pending => break,
            PrefetchChunkResult::RestartRequired => {
                queue.close(PlaybackQueueCloseReason::RestartRequired);
                break;
            }
        }
    }

    let source = TimeshiftPlaybackSource::new(
        queue.clone(),
        clock,
        stop_requested.clone(),
        spectrum_bits,
        initial_chunk,
    );
    let worker_handle = {
        let store = store.clone();
        let notifier = notifier.clone();
        let queue = queue.clone();
        let event_tx = event_tx.clone();
        thread::spawn(move || {
            run_playback_prefetch(
                store,
                notifier,
                queue,
                event_tx,
                stop_requested,
                cursor,
                expected_channels,
                expected_sample_rate,
                fft_state,
            )
        })
    };

    Ok(Some(PlaybackStart {
        queue,
        source,
        worker_handle,
    }))
}

fn teardown_playback_pipeline(
    sink: &Player,
    live_buffer: &Arc<LiveChunkBuffer>,
    notifier: &Arc<PlaybackNotifier>,
    playback_queue: &mut Option<Arc<PlaybackQueue>>,
    playback_worker: &mut Option<thread::JoinHandle<()>>,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
) {
    sink.stop();
    if let Some(queue) = playback_queue.take() {
        queue.close(PlaybackQueueCloseReason::Stopped);
    }
    live_buffer.wake();
    notifier.notify();
    if let Some(handle) = playback_worker.take() {
        let _ = handle.join();
    }
    clear_spectrum(spectrum_bits);
}

fn run_store_writer(
    store: Arc<Mutex<TimeshiftStore>>,
    live_buffer: Arc<LiveChunkBuffer>,
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
                let (start_ms, end_ms) =
                    store.append_pcm(sample_rate, channels, &samples, now_ms)?;
                clock.set_live_head_ms(end_ms);

                let floor = store.earliest_timestamp_ms();
                let live_head = store.live_head_ms();
                drop(store);

                live_buffer.push(StoredPcmChunk {
                    channels,
                    sample_rate,
                    samples,
                    start_ms,
                    end_ms,
                });

                if clock.playback_cursor_ms() == 0 {
                    clock.set_playback_cursor_ms(buffered_playback_start_ms(live_head, floor));
                } else if let Some(floor) = floor {
                    clock.clamp_playback_floor(floor);
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
    stop_requested: &Arc<AtomicBool>,
    store_tx: &mpsc::Sender<StoreCommand>,
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
                continue;
            }
            Err(err) => {
                eprintln!("Error reading packet: {err:?}");
                return Ok(RunOutcome::Reconnect);
            }
        };

        let (outcome, audio) = decode_and_process_packet(
            &packet,
            format,
            track_id,
            decoder,
            decoder_opts,
            false,
            spectrum_bits,
            &mut decode_state,
            fft_state,
            VizParams {
                peak_attack: 0.08,
                peak_release: 0.998,
                sensitivity: 1.25,
                curve: 0.75,
            },
        )?;

        match outcome {
            PacketOutcome::Continue => {}
            PacketOutcome::Reconnect => return Ok(RunOutcome::Reconnect),
            PacketOutcome::SpecChanged => continue,
        }

        if let Some((channels, sample_rate, samples)) = audio {
            if store_tx
                .send(StoreCommand::Append {
                    sample_rate,
                    channels,
                    samples,
                    now_ms: now_timestamp_ms(),
                })
                .is_err()
            {
                return Ok(RunOutcome::Stop);
            }
        }
    }
}

fn run_ingest_loop(
    station: Station,
    stop_requested: Arc<AtomicBool>,
    store_tx: mpsc::Sender<StoreCommand>,
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

        match run_one_connection(
            &mut format,
            &mut track_id,
            &mut decoder,
            &decoder_opts,
            &stop_requested,
            &store_tx,
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
    let store = Arc::new(Mutex::new(TimeshiftStore::new_session(
        root.clone(),
        RETENTION_MS,
    )?));
    let live_buffer = Arc::new(LiveChunkBuffer::default());
    let notifier = Arc::new(PlaybackNotifier::default());
    clock.reset();

    let stop_requested = Arc::new(AtomicBool::new(false));
    let (store_tx, store_rx) = mpsc::channel();
    let store_handle = {
        let store = store.clone();
        let live_buffer = live_buffer.clone();
        let clock = clock.clone();
        let notifier = notifier.clone();
        let stop_requested = stop_requested.clone();
        thread::spawn(move || {
            let writer_stop_requested = stop_requested.clone();
            if let Err(err) = run_store_writer(
                store,
                live_buffer,
                clock,
                notifier,
                writer_stop_requested,
                store_rx,
            ) {
                eprintln!("timeshift writer error: {err}");
                stop_requested.store(true, Ordering::Relaxed);
            }
        })
    };
    let ingest_handle = {
        let stop_requested = stop_requested.clone();
        let store_tx = store_tx.clone();
        let spectrum_bits = spectrum_bits.clone();
        thread::spawn(move || run_ingest_loop(station, stop_requested, store_tx, spectrum_bits))
    };

    let mut stream = DeviceSinkBuilder::open_default_sink()?;
    stream.log_on_drop(false);
    let sink = Player::connect_new(stream.mixer());
    let (playback_event_tx, playback_event_rx) = mpsc::channel();
    let mut playback_queue = None;
    let mut playback_worker = None;
    let mut paused = false;
    let mut source_started = false;

    loop {
        match handle_output_control(&rx, &sink, &mut paused, &stop_requested, &spectrum_bits)? {
            OutputCommandResult::Continue => {}
            OutputCommandResult::Stop => break,
            OutputCommandResult::RestartPlayback => {
                source_started = false;
                teardown_playback_pipeline(
                    &sink,
                    &live_buffer,
                    &notifier,
                    &mut playback_queue,
                    &mut playback_worker,
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
                        &sink,
                        &live_buffer,
                        &notifier,
                        &mut playback_queue,
                        &mut playback_worker,
                        &spectrum_bits,
                    );
                }
            }
        }

        if !paused && !source_started {
            if let Some(playback) = try_start_playback(
                &store,
                &live_buffer,
                notifier.clone(),
                clock.clone(),
                stop_requested.clone(),
                spectrum_bits.clone(),
                playback_event_tx.clone(),
            )? {
                let PlaybackStart {
                    queue,
                    source,
                    worker_handle,
                } = playback;
                playback_queue = Some(queue);
                sink.append(source);
                playback_worker = Some(worker_handle);
                source_started = true;
                continue;
            }
        }

        match rx.recv_timeout(Duration::from_millis(OUTPUT_WAIT_TIMEOUT_MS)) {
            Ok(cmd) => {
                match handle_output_command(
                    cmd,
                    &sink,
                    &mut paused,
                    &stop_requested,
                    &spectrum_bits,
                )? {
                    OutputCommandResult::Continue => {}
                    OutputCommandResult::Stop => break,
                    OutputCommandResult::RestartPlayback => {
                        source_started = false;
                        teardown_playback_pipeline(
                            &sink,
                            &live_buffer,
                            &notifier,
                            &mut playback_queue,
                            &mut playback_worker,
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
    drop(store_tx);
    teardown_playback_pipeline(
        &sink,
        &live_buffer,
        &notifier,
        &mut playback_queue,
        &mut playback_worker,
        &spectrum_bits,
    );
    let _ = ingest_handle.join();
    let _ = store_handle.join();
    store.lock().expect("timeshift mutex poisoned").clear()?;
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
    use super::{buffered_playback_start_ms, coalesce_playback_chunks, slice_chunk_from_ms};
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
    fn slice_chunk_from_cursor_preserves_remaining_samples() {
        let chunk = StoredPcmChunk {
            channels: 2,
            sample_rate: 1_000,
            samples: (0..20).map(|value| value as f32).collect(),
            start_ms: 0,
            end_ms: 5,
        };

        let sliced = slice_chunk_from_ms(&chunk, 2).expect("slice");
        assert_eq!(sliced.start_ms, 2);
        assert_eq!(sliced.end_ms, 5);
        assert_eq!(
            sliced.samples,
            (8..20).map(|value| value as f32).collect::<Vec<_>>()
        );
    }

    #[test]
    fn coalesce_chunks_merges_adjacent_audio_up_to_target_duration() {
        let chunks = vec![
            StoredPcmChunk {
                channels: 2,
                sample_rate: 1_000,
                samples: vec![0.0; 400],
                start_ms: 0,
                end_ms: 100,
            },
            StoredPcmChunk {
                channels: 2,
                sample_rate: 1_000,
                samples: vec![1.0; 400],
                start_ms: 100,
                end_ms: 200,
            },
            StoredPcmChunk {
                channels: 2,
                sample_rate: 1_000,
                samples: vec![2.0; 400],
                start_ms: 200,
                end_ms: 300,
            },
        ];

        let merged = coalesce_playback_chunks(chunks, 1_000);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].start_ms, 0);
        assert_eq!(merged[0].end_ms, 300);
        assert_eq!(merged[0].samples.len(), 1_200);
    }
}
