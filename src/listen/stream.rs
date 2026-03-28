use reqwest::blocking::Client;
use rodio::{buffer::SamplesBuffer, DeviceSinkBuilder, Player, Source};
use std::num::{NonZeroU16, NonZeroU32};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{mpsc, Arc, Mutex};
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
use super::store::{clear_root, TimeshiftStore, RETENTION_MS};
use super::viz::{
    clear_spectrum, decode_and_process_packet, make_fft_state, process_samples_for_viz,
    reset_fft_state, DecodeState, FftVizState, PacketOutcome, VizParams,
};
use super::{Control, Result};

#[derive(Debug, Clone, Copy)]
enum RunOutcome {
    Stop,
    Reconnect,
}

const OUTPUT_CHUNK_MS: u64 = 50;
const OUTPUT_QUEUE_TARGET_CHUNKS: usize = 4;
const OUTPUT_WAIT_TIMEOUT_MS: u64 = 10;

struct NotifyingSamplesBuffer {
    inner: SamplesBuffer,
    chunk_end_ms: u64,
    done_tx: mpsc::Sender<u64>,
    did_notify: bool,
}

impl NotifyingSamplesBuffer {
    fn new(inner: SamplesBuffer, chunk_end_ms: u64, done_tx: mpsc::Sender<u64>) -> Self {
        Self {
            inner,
            chunk_end_ms,
            done_tx,
            did_notify: false,
        }
    }

    fn notify_done(&mut self) {
        if self.did_notify {
            return;
        }

        self.did_notify = true;
        let _ = self.done_tx.send(self.chunk_end_ms);
    }
}

impl Iterator for NotifyingSamplesBuffer {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        match self.inner.next() {
            Some(sample) => Some(sample),
            None => {
                self.notify_done();
                None
            }
        }
    }
}

impl Source for NotifyingSamplesBuffer {
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
) -> Result<bool> {
    match cmd {
        Control::Stop => {
            if is_verbose() {
                println!("[{}] Stop requested, shutting down stream.", now_string());
            }
            stop_requested.store(true, Ordering::Relaxed);
            sink.stop();
            clear_spectrum(spectrum_bits);
            return Ok(true);
        }
        Control::Pause => {
            if !*paused {
                if is_verbose() {
                    println!("[{}] Pausing playback.", now_string());
                }
                *paused = true;
                sink.pause();
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
            }
        }
    }

    Ok(false)
}

fn handle_output_control(
    rx: &mpsc::Receiver<Control>,
    sink: &Player,
    paused: &mut bool,
    stop_requested: &Arc<AtomicBool>,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
) -> Result<bool> {
    while let Ok(cmd) = rx.try_recv() {
        if handle_output_command(cmd, sink, paused, stop_requested, spectrum_bits)? {
            return Ok(true);
        }
    }

    Ok(false)
}

fn run_one_connection(
    format: &mut Box<dyn symphonia::core::formats::FormatReader>,
    track_id: &mut u32,
    decoder: &mut Box<dyn symphonia::core::codecs::Decoder>,
    decoder_opts: &DecoderOptions,
    stop_requested: &Arc<AtomicBool>,
    store: &Arc<Mutex<TimeshiftStore>>,
    clock: &Arc<PlaybackClock>,
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
                reset_fft_state(
                    &mut fft_state.mono_ring,
                    &mut fft_state.bars_smooth,
                    &mut fft_state.bar_peak,
                    spectrum_bits,
                );
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
            let now_ms = now_timestamp_ms();
            let mut store = store.lock().expect("timeshift mutex poisoned");
            let (_, end_ms) = store.append_pcm(sample_rate, channels, &samples, now_ms)?;
            clock.set_live_head_ms(end_ms);

            let floor = store.earliest_timestamp_ms();
            let live_head = store.live_head_ms();
            drop(store);

            if clock.playback_cursor_ms() == 0 {
                clock.set_playback_cursor_ms(live_head);
            } else if let Some(floor) = floor {
                clock.clamp_playback_floor(floor);
            }
        }
    }
}

fn run_ingest_loop(
    station: Station,
    stop_requested: Arc<AtomicBool>,
    store: Arc<Mutex<TimeshiftStore>>,
    clock: Arc<PlaybackClock>,
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
            &store,
            &clock,
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
    clock.reset();

    let stop_requested = Arc::new(AtomicBool::new(false));
    let ingest_handle = {
        let stop_requested = stop_requested.clone();
        let store = store.clone();
        let clock = clock.clone();
        let spectrum_bits = spectrum_bits.clone();
        thread::spawn(move || run_ingest_loop(station, stop_requested, store, clock, spectrum_bits))
    };

    let mut stream = DeviceSinkBuilder::open_default_sink()?;
    stream.log_on_drop(false);
    let sink = Player::connect_new(stream.mixer());
    let mut paused = false;
    let mut fft_state = make_fft_state(spectrum_bits.len());
    let (chunk_done_tx, chunk_done_rx) = mpsc::channel();
    let mut queued_cursor_ms = 0u64;
    let viz = VizParams {
        peak_attack: 0.08,
        peak_release: 0.998,
        sensitivity: 1.25,
        curve: 0.75,
    };

    loop {
        while let Ok(end_ms) = chunk_done_rx.try_recv() {
            clock.set_playback_cursor_ms(end_ms);
        }

        if handle_output_control(&rx, &sink, &mut paused, &stop_requested, &spectrum_bits)? {
            break;
        }

        if paused {
            match rx.recv() {
                Ok(cmd) => {
                    if handle_output_command(
                        cmd,
                        &sink,
                        &mut paused,
                        &stop_requested,
                        &spectrum_bits,
                    )? {
                        break;
                    }
                }
                Err(_) => break,
            }
            continue;
        }

        let mut appended_chunk = false;

        while !paused && sink.len() < OUTPUT_QUEUE_TARGET_CHUNKS {
            let chunk = {
                let store = store.lock().expect("timeshift mutex poisoned");
                let requested_cursor_ms = if queued_cursor_ms == 0 {
                    clock.playback_cursor_ms()
                } else {
                    queued_cursor_ms
                };
                let cursor_ms = store.clamp_cursor_ms(requested_cursor_ms);
                if cursor_ms != requested_cursor_ms {
                    queued_cursor_ms = cursor_ms;
                    if sink.empty() {
                        clock.set_playback_cursor_ms(cursor_ms);
                    }
                }

                let live_head = store.live_head_ms();
                if live_head == 0 {
                    None
                } else if cursor_ms == 0 {
                    queued_cursor_ms = live_head;
                    None
                } else {
                    match store.read_chunk(cursor_ms, OUTPUT_CHUNK_MS)? {
                        Some(chunk) => Some(chunk),
                        None => {
                            if let Some(next) = store.next_available_timestamp_ms(cursor_ms) {
                                queued_cursor_ms = next;
                                if sink.empty() {
                                    clock.set_playback_cursor_ms(next);
                                }
                            }
                            None
                        }
                    }
                }
            };

            let Some(chunk) = chunk else {
                break;
            };

            let super::store::StoredPcmChunk {
                channels,
                sample_rate,
                samples,
                end_ms,
                ..
            } = chunk;

            process_samples_for_viz(
                &samples,
                channels,
                sample_rate,
                true,
                &spectrum_bits,
                &mut fft_state,
                viz,
            );
            append_chunk(
                &sink,
                channels,
                sample_rate,
                samples,
                end_ms,
                &chunk_done_tx,
            );
            queued_cursor_ms = end_ms;
            appended_chunk = true;
        }

        if appended_chunk {
            continue;
        }

        if sink.empty() {
            match rx.recv_timeout(Duration::from_millis(OUTPUT_WAIT_TIMEOUT_MS)) {
                Ok(cmd) => {
                    if handle_output_command(
                        cmd,
                        &sink,
                        &mut paused,
                        &stop_requested,
                        &spectrum_bits,
                    )? {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match chunk_done_rx.recv_timeout(Duration::from_millis(OUTPUT_WAIT_TIMEOUT_MS)) {
                Ok(end_ms) => {
                    clock.set_playback_cursor_ms(end_ms);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    stop_requested.store(true, Ordering::Relaxed);
    let _ = ingest_handle.join();
    clear_spectrum(&spectrum_bits);
    store.lock().expect("timeshift mutex poisoned").clear()?;
    clear_root(&root)?;

    Ok(())
}

fn append_chunk(
    sink: &Player,
    channels: u16,
    sample_rate: u32,
    samples: Vec<f32>,
    end_ms: u64,
    chunk_done_tx: &mpsc::Sender<u64>,
) {
    let Some(channels) = NonZeroU16::new(channels) else {
        return;
    };
    let Some(sample_rate) = NonZeroU32::new(sample_rate) else {
        return;
    };

    let source = NotifyingSamplesBuffer::new(
        SamplesBuffer::new(channels, sample_rate, samples),
        end_ms,
        chunk_done_tx.clone(),
    );
    sink.append(source);
}

fn now_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
