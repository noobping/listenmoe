use reqwest::blocking::Client;
use rodio::{buffer::SamplesBuffer, queue, DeviceSinkBuilder};
use std::num::{NonZeroU16, NonZeroU32};
use std::sync::atomic::AtomicU32;
use std::sync::{mpsc, Arc};
use std::time::{SystemTime, UNIX_EPOCH};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::http_source::HttpSource;
use crate::log::{is_verbose, now_string};
use crate::station::Station;

use super::super::clock::PlaybackClock;
use super::super::viz::{
    clear_spectrum, decode_and_process_packet, make_fft_state, reset_fft_state, DecodeState,
    PacketOutcome, VizParams,
};
use super::super::{Control, Result};

const LIVE_CLOCK_FLUSH_MS: u64 = 250;

pub(in crate::listen) fn run_listenmoe_stream(
    station: Station,
    rx: mpsc::Receiver<Control>,
    spectrum_bits: Arc<Vec<AtomicU32>>,
    clock: Arc<PlaybackClock>,
) -> Result<()> {
    clock.reset();
    clock.set_live_playback(true);
    let mut stream = DeviceSinkBuilder::open_default_sink()?;
    stream.log_on_drop(false);
    let result = run_direct_live_until_stop(station, &rx, &spectrum_bits, &clock, stream.mixer());
    clock.set_live_playback(false);
    result
}

fn run_direct_live_until_stop(
    station: Station,
    rx: &mpsc::Receiver<Control>,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
    clock: &Arc<PlaybackClock>,
    mixer: &rodio::mixer::Mixer,
) -> Result<()> {
    let primary = station.stream_url().to_string();
    let fallback = station.stream_fallback_url().to_string();
    let mut use_fallback = false;

    let mut client = build_client()?;
    let useragent = build_useragent();

    let format_opts: FormatOptions = Default::default();
    let metadata_opts: MetadataOptions = Default::default();
    let decoder_opts: DecoderOptions = Default::default();

    let (mut sink, sink_source) = queue::queue(true);
    mixer.add(sink_source);
    let mut fft_state = make_fft_state(spectrum_bits.len());
    let mut live_clock = LiveClockTracker::new(clock.live_head_ms());

    loop {
        if handle_stop_control(rx, &sink, spectrum_bits)? {
            live_clock.flush_now(clock);
            return Ok(());
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

        sink.set_keep_alive_if_empty(false);
        sink.clear();
        let (new_sink, sink_source) = queue::queue(true);
        mixer.add(sink_source);
        sink = new_sink;
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
            if handle_stop_control(rx, &sink, spectrum_bits)? {
                live_clock.flush_now(clock);
                return Ok(());
            }

            let packet = match format.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::ResetRequired) => {
                    if is_verbose() {
                        println!(
                            "[{}] Stream reset, reconfiguring live decoder...",
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
                live_viz_params(),
            )?;

            match outcome {
                PacketOutcome::Continue => {}
                PacketOutcome::Reconnect => break,
                PacketOutcome::SpecChanged => {
                    sink.set_keep_alive_if_empty(false);
                    sink.clear();
                    let (new_sink, sink_source) = queue::queue(true);
                    mixer.add(sink_source);
                    sink = new_sink;
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
                let Some(channels) = NonZeroU16::new(channels) else {
                    continue;
                };
                let Some(sample_rate) = NonZeroU32::new(sample_rate) else {
                    continue;
                };
                let sample_count = samples.len();
                sink.append(SamplesBuffer::new(channels, sample_rate, samples));
                if live_clock
                    .advance(sample_rate.get(), channels.get(), sample_count)
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

fn handle_stop_control(
    rx: &mpsc::Receiver<Control>,
    sink: &Arc<queue::SourcesQueueInput>,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
) -> Result<bool> {
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            Control::Stop => {
                if is_verbose() {
                    println!(
                        "[{}] Stop requested, shutting down live stream.",
                        now_string()
                    );
                }
                sink.set_keep_alive_if_empty(false);
                sink.clear();
                clear_spectrum(spectrum_bits);
                return Ok(true);
            }
        }
    }

    Ok(false)
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
        println!("[{}] Connecting to {url}...", now_string());
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
    let mut hint = Hint::new();
    hint.with_extension("ogg");

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

fn build_client() -> Result<Client> {
    Ok(Client::new())
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

fn now_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
