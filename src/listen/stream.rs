use reqwest::blocking::Client;
use rodio::{buffer::SamplesBuffer, DeviceSinkBuilder, Player};
use std::num::{NonZeroU16, NonZeroU32};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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

const OUTPUT_CHUNK_MS: u64 = 10;
const OUTPUT_IDLE_SLEEP_MS: u64 = 10;

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

fn handle_output_control(
    rx: &mpsc::Receiver<Control>,
    sink: &Player,
    paused: &mut bool,
    stop_requested: &Arc<AtomicBool>,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
) -> Result<bool> {
    while let Ok(cmd) = rx.try_recv() {
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
                peak_attack: 0.12,
                peak_release: 0.995,
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
                thread::sleep(Duration::from_secs(1));
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
    let viz = VizParams {
        peak_attack: 0.12,
        peak_release: 0.995,
        sensitivity: 1.25,
        curve: 0.75,
    };

    loop {
        if handle_output_control(&rx, &sink, &mut paused, &stop_requested, &spectrum_bits)? {
            break;
        }

        if paused {
            thread::sleep(Duration::from_millis(OUTPUT_IDLE_SLEEP_MS));
            continue;
        }

        let chunk = {
            let store = store.lock().expect("timeshift mutex poisoned");
            let cursor_ms = store.clamp_cursor_ms(clock.playback_cursor_ms());
            if cursor_ms != clock.playback_cursor_ms() {
                clock.set_playback_cursor_ms(cursor_ms);
            }

            let live_head = store.live_head_ms();
            if live_head == 0 {
                None
            } else if clock.playback_cursor_ms() == 0 {
                clock.set_playback_cursor_ms(live_head);
                None
            } else {
                match store.read_chunk(clock.playback_cursor_ms(), OUTPUT_CHUNK_MS)? {
                    Some(chunk) => Some(chunk),
                    None => {
                        if let Some(next) =
                            store.next_available_timestamp_ms(clock.playback_cursor_ms())
                        {
                            clock.set_playback_cursor_ms(next);
                        }
                        None
                    }
                }
            }
        };

        let Some(chunk) = chunk else {
            thread::sleep(Duration::from_millis(OUTPUT_IDLE_SLEEP_MS));
            continue;
        };

        append_chunk(&sink, &chunk);
        process_samples_for_viz(
            &chunk.samples,
            chunk.channels,
            chunk.sample_rate,
            true,
            &spectrum_bits,
            &mut fft_state,
            viz,
        );
        clock.set_playback_cursor_ms(chunk.end_ms);

        let sleep_ms = chunk.end_ms.saturating_sub(chunk.start_ms).max(1);
        thread::sleep(Duration::from_millis(sleep_ms));
    }

    stop_requested.store(true, Ordering::Relaxed);
    let _ = ingest_handle.join();
    clear_spectrum(&spectrum_bits);
    store.lock().expect("timeshift mutex poisoned").clear()?;
    clear_root(&root)?;

    Ok(())
}

fn append_chunk(sink: &Player, chunk: &super::store::StoredPcmChunk) {
    let Some(channels) = NonZeroU16::new(chunk.channels) else {
        return;
    };
    let Some(sample_rate) = NonZeroU32::new(chunk.sample_rate) else {
        return;
    };

    sink.append(SamplesBuffer::new(
        channels,
        sample_rate,
        chunk.samples.clone(),
    ));
}

fn now_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
