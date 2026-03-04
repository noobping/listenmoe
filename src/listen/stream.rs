use reqwest::blocking::Client;
use rodio::{buffer::SamplesBuffer, DeviceSinkBuilder, Player};
use std::num::{NonZeroU16, NonZeroU32};
use std::sync::{atomic::AtomicU32, mpsc, Arc};
use std::time::Duration;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::http_source::HttpSource;
#[cfg(debug_assertions)]
use crate::log::now_string;
use crate::station::Station;

use super::viz::{
    clear_spectrum, decode_and_process_packet, make_fft_state, reset_fft_state, DecodeState,
    FftVizState, PacketOutcome, VizParams,
};
use super::{Control, Result};

#[derive(Debug, Clone, Copy)]
enum RunOutcome {
    Stop,
    Reconnect,
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
    #[cfg(debug_assertions)]
    println!("[{}] Connecting to {url}…", now_string());

    let response = client.get(url).header("User-Agent", useragent).send()?;
    #[cfg(debug_assertions)]
    println!("[{}] HTTP status: {}", now_string(), response.status());

    if !response.status().is_success() {
        return Err(format!("HTTP status {}", response.status()).into());
    }

    let http_source = HttpSource { inner: response };
    let mss = MediaSourceStream::new(Box::new(http_source), Default::default());

    let hint = Hint::new(); // let symphonia probe

    let probed = symphonia::default::get_probe().format(&hint, mss, format_opts, metadata_opts)?;

    let format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "no supported audio tracks".to_string())?;

    let track_id = track.id;
    let decoder = symphonia::default::get_codecs().make(&track.codec_params, decoder_opts)?;

    Ok((format, track_id, decoder))
}

fn handle_control(
    rx: &mpsc::Receiver<Control>,
    sink: &Player,
    paused: &mut bool,
    bars_enabled: &mut bool,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
) -> Result<bool> {
    // returns Ok(true) if Stop requested
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            Control::Stop => {
                #[cfg(debug_assertions)]
                println!("[{}] Stop requested, shutting down stream.", now_string());
                sink.stop();
                return Ok(true);
            }
            Control::Pause => {
                if !*paused {
                    #[cfg(debug_assertions)]
                    println!("[{}] Pausing playback.", now_string());
                    *paused = true;
                    sink.pause();
                }
                *bars_enabled = false;
                clear_spectrum(spectrum_bits);
            }
            Control::Resume => {
                if *paused {
                    #[cfg(debug_assertions)]
                    println!("[{}] Resuming playback.", now_string());
                    *paused = false;
                    sink.play();
                    *bars_enabled = true;
                }
            }
        }
    }
    Ok(false)
}

fn run_one_connection(
    rx: &mpsc::Receiver<Control>,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
    format: &mut Box<dyn symphonia::core::formats::FormatReader>,
    track_id: &mut u32,
    decoder: &mut Box<dyn symphonia::core::codecs::Decoder>,
    decoder_opts: &DecoderOptions,
    sink: &Player,
    paused: &mut bool,
    bars_enabled: &mut bool,
    fft_state: &mut FftVizState,
    viz: VizParams,
) -> Result<RunOutcome> {
    let mut decode_state = DecodeState {
        sample_buf: None,
        channels: 0,
        sample_rate: 0,
    };

    loop {
        if handle_control(rx, sink, paused, bars_enabled, spectrum_bits)? {
            return Ok(RunOutcome::Stop);
        }

        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::ResetRequired) => {
                #[cfg(debug_assertions)]
                println!("[{}] Stream reset, reconfiguring decoder…", now_string());

                let new_track = format
                    .tracks()
                    .iter()
                    .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
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
            *bars_enabled,
            spectrum_bits,
            &mut decode_state,
            fft_state,
            viz,
        )?;

        match outcome {
            PacketOutcome::Continue => {}
            PacketOutcome::Reconnect => return Ok(RunOutcome::Reconnect),
            PacketOutcome::SpecChanged { .. } => {
                // Recreate sink on spec change
                sink.stop();
                if *paused {
                    sink.pause();
                }

                reset_fft_state(
                    &mut fft_state.mono_ring,
                    &mut fft_state.bars_smooth,
                    &mut fft_state.bar_peak,
                    spectrum_bits,
                );

                // Continue; next decoded buffer will create a new SampleBuffer and then deliver audio.
                continue;
            }
        }

        if let Some((channels, sample_rate, samples)) = audio {
            append_samples_in_chunks(sink, channels, sample_rate, &samples); // send audio to rodio
        }
    }
}

pub(super) fn run_listenmoe_stream(
    station: Station,
    rx: mpsc::Receiver<Control>,
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

    let mut stream = DeviceSinkBuilder::open_default_sink()?;
    stream.log_on_drop(false);
    let mut sink = Player::connect_new(stream.mixer());

    let mut paused = false;
    let mut bars_enabled = true;

    let mut fft_state = make_fft_state(spectrum_bits.len());
    let viz = VizParams {
        peak_attack: 0.35,
        peak_release: 0.995,
        sensitivity: 1.25,
        curve: 0.75,
    };

    loop {
        let url: &str = if use_fallback { &fallback } else { &primary };

        let (mut format, mut track_id, mut decoder) = match open_stream(
            url,
            &client,
            &useragent,
            &format_opts,
            &metadata_opts,
            &decoder_opts,
        ) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("connect/probe error on {url}: {e}");
                if !fallback.is_empty() {
                    use_fallback = !use_fallback;
                }
                client = build_client()?;
                continue;
            }
        };

        // On reconnect: clear sink queue + reset viz
        sink.stop();
        sink = Player::connect_new(stream.mixer());
        reset_fft_state(
            &mut fft_state.mono_ring,
            &mut fft_state.bars_smooth,
            &mut fft_state.bar_peak,
            &spectrum_bits,
        );

        #[cfg(debug_assertions)]
        println!("[{}] Started decoding + playback.", now_string());

        let outcome = run_one_connection(
            &rx,
            &spectrum_bits,
            &mut format,
            &mut track_id,
            &mut decoder,
            &decoder_opts,
            &sink,
            &mut paused,
            &mut bars_enabled,
            &mut fft_state,
            viz,
        )?;

        match outcome {
            RunOutcome::Stop => return Ok(()),
            RunOutcome::Reconnect => {
                if !fallback.is_empty() {
                    use_fallback = !use_fallback;
                }
                continue;
            }
        }
    }
}

fn append_samples_in_chunks(sink: &Player, channels: u16, sample_rate: u32, samples: &[f32]) {
    // 10ms chunks (tweak to 5..20ms)
    const CHUNK_MS: u32 = 10;

    let ch = channels as usize;
    if ch == 0 || sample_rate == 0 {
        return;
    }

    let Some(channels) = NonZeroU16::new(channels) else {
        return;
    };
    let Some(sample_rate) = NonZeroU32::new(sample_rate) else {
        return;
    };

    // frames per chunk = sr * ms / 1000
    let frames_per_chunk = (sample_rate.get() * CHUNK_MS / 1000).max(1) as usize;
    let samples_per_chunk = frames_per_chunk * ch;

    for chunk in samples.chunks(samples_per_chunk) {
        // This clones each small chunk into rodio; contents unchanged.
        sink.append(SamplesBuffer::new(channels, sample_rate, chunk.to_vec()));
    }
}
