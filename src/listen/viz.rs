use rustfft::{num_complex::Complex32, FftPlanner};
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};
use symphonia::core::audio::{AudioBufferRef, SampleBuffer};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;

use crate::log::{is_verbose, now_string};

use super::Result;

const FFT_SIZE: usize = 1024;
const HOP: usize = 512;

pub(super) struct FftVizState {
    pub(super) mono_ring: Vec<f32>,
    pub(super) fft_in: Vec<Complex32>,
    pub(super) mags: Vec<f32>,
    pub(super) bars: Vec<f32>,
    pub(super) bars_smooth: Vec<f32>,
    pub(super) bar_peak: Vec<f32>,
    pub(super) window: Vec<f32>,
    pub(super) fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
}

pub(super) struct DecodeState {
    pub(super) sample_buf: Option<SampleBuffer<f32>>,
    pub(super) channels: u16,
    pub(super) sample_rate: u32,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct VizParams {
    pub(super) peak_attack: f32,
    pub(super) peak_release: f32,
    pub(super) sensitivity: f32,
    pub(super) curve: f32,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum PacketOutcome {
    Continue,
    Reconnect,
    SpecChanged,
}

pub(super) fn make_fft_state(num_bars: usize) -> FftVizState {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);

    FftVizState {
        mono_ring: Vec::with_capacity(FFT_SIZE * 4),
        fft_in: vec![Complex32::new(0.0, 0.0); FFT_SIZE],
        mags: vec![0.0; FFT_SIZE / 2],
        bars: vec![0.0; num_bars],
        bars_smooth: vec![0.0; num_bars],
        bar_peak: vec![0.0; num_bars],
        window: hann_window(FFT_SIZE),
        fft,
    }
}

/// Returns:
/// - PacketOutcome::Continue + Some((ch, sr, samples)) when audio is ready
/// - PacketOutcome::SpecChanged when SR/ch changed (caller should recreate sink + reset FFT)
/// - PacketOutcome::Reconnect on fatal decode
pub(super) fn decode_and_process_packet(
    packet: &symphonia::core::formats::Packet,
    format: &mut Box<dyn symphonia::core::formats::FormatReader>,
    track_id: &mut u32,
    decoder: &mut Box<dyn symphonia::core::codecs::Decoder>,
    decoder_opts: &DecoderOptions,
    bars_enabled: bool,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
    decode_state: &mut DecodeState,
    fft_state: &mut FftVizState,
    viz: VizParams,
) -> Result<(PacketOutcome, Option<(u16, u32, Vec<f32>)>)> {
    if packet.track_id() != *track_id {
        return Ok((PacketOutcome::Continue, None));
    }

    let decoded: AudioBufferRef<'_> = match decoder.decode(packet) {
        Ok(buf) => buf,
        Err(SymphoniaError::DecodeError(_)) => return Ok((PacketOutcome::Continue, None)),
        Err(SymphoniaError::ResetRequired) => {
            if is_verbose() {
                println!(
                    "[{}] Decoder reset required, rebuilding decoder…",
                    now_string()
                );
            }

            let new_track = format
                .tracks()
                .iter()
                .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
                .ok_or_else(|| "no supported audio tracks after decoder reset".to_string())?;

            *track_id = new_track.id;
            *decoder =
                symphonia::default::get_codecs().make(&new_track.codec_params, decoder_opts)?;

            decode_state.sample_buf = None;
            if bars_enabled {
                reset_fft_state(
                    &mut fft_state.mono_ring,
                    &mut fft_state.bars_smooth,
                    &mut fft_state.bar_peak,
                    spectrum_bits,
                );
            } else {
                fft_state.mono_ring.clear();
                fft_state.bars_smooth.fill(0.0);
                fft_state.bar_peak.fill(0.0);
            }
            return Ok((PacketOutcome::Continue, None));
        }
        Err(err) => {
            eprintln!("Fatal decode error: {err:?}");
            return Ok((PacketOutcome::Reconnect, None));
        }
    };

    // Detect spec changes
    let spec = *decoded.spec();
    let new_channels = spec.channels.count() as u16;
    let new_rate = spec.rate;

    let first_time = decode_state.sample_buf.is_none();
    let spec_changed = !first_time
        && (new_channels != decode_state.channels || new_rate != decode_state.sample_rate);

    if first_time || spec_changed {
        decode_state.channels = new_channels;
        decode_state.sample_rate = new_rate;

        let duration = decoded.capacity() as u64;
        decode_state.sample_buf = Some(SampleBuffer::<f32>::new(duration, spec));

        if spec_changed {
            return Ok((PacketOutcome::SpecChanged, None));
        }
    }

    let buf = decode_state
        .sample_buf
        .as_mut()
        .expect("sample_buf must be initialized");
    buf.copy_interleaved_ref(decoded);
    let samples = buf.samples().to_owned();

    if bars_enabled {
        process_samples_for_viz(
            &samples,
            decode_state.channels,
            decode_state.sample_rate,
            true,
            spectrum_bits,
            fft_state,
            viz,
        );
    }

    Ok((
        PacketOutcome::Continue,
        Some((decode_state.channels, decode_state.sample_rate, samples)),
    ))
}

pub(super) fn process_samples_for_viz(
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
    bars_enabled: bool,
    spectrum_bits: &Arc<Vec<AtomicU32>>,
    fft_state: &mut FftVizState,
    viz: VizParams,
) {
    process_samples_for_viz_inner(samples, channels, sample_rate, fft_state, viz);

    if bars_enabled {
        apply_spectrum_snapshot(spectrum_bits, &build_spectrum_snapshot_bits(fft_state));
    } else {
        clear_spectrum(spectrum_bits);
    }
}

pub(super) fn build_spectrum_snapshot(
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
    fft_state: &mut FftVizState,
    viz: VizParams,
) -> Vec<u32> {
    process_samples_for_viz_inner(samples, channels, sample_rate, fft_state, viz);
    fft_state
        .bars_smooth
        .iter()
        .map(|value| value.to_bits())
        .collect()
}

fn build_spectrum_snapshot_bits(fft_state: &FftVizState) -> Vec<u32> {
    fft_state
        .bars_smooth
        .iter()
        .map(|value| value.to_bits())
        .collect()
}

pub(super) fn apply_spectrum_snapshot(spectrum_bits: &Arc<Vec<AtomicU32>>, snapshot: &[u32]) {
    for (target, value) in spectrum_bits.iter().zip(snapshot.iter().copied()) {
        target.store(value, Ordering::Relaxed);
    }

    for target in spectrum_bits.iter().skip(snapshot.len()) {
        target.store(0.0f32.to_bits(), Ordering::Relaxed);
    }
}

fn process_samples_for_viz_inner(
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
    fft_state: &mut FftVizState,
    viz: VizParams,
) {
    let ch = channels as usize;
    if ch > 0 {
        let frames = samples.len() / ch;
        fft_state.mono_ring.reserve(frames);

        for f in 0..frames {
            let mut acc = 0.0f32;
            for c in 0..ch {
                acc += samples[f * ch + c];
            }
            fft_state.mono_ring.push(acc / (ch as f32));
        }
    }

    while fft_state.mono_ring.len() >= FFT_SIZE {
        for i in 0..FFT_SIZE {
            let x = fft_state.mono_ring[i] * fft_state.window[i];
            fft_state.fft_in[i] = Complex32::new(x, 0.0);
        }

        fft_state.fft.process(&mut fft_state.fft_in);

        for i in 0..(FFT_SIZE / 2) {
            let c = fft_state.fft_in[i];
            fft_state.mags[i] = (c.re * c.re + c.im * c.im).sqrt();
        }

        bins_to_bars(&fft_state.mags, sample_rate, &mut fft_state.bars);

        for i in 0..fft_state.bars.len() {
            let v = fft_state.bars[i].max(1e-12);

            if fft_state.bar_peak[i] == 0.0 {
                fft_state.bar_peak[i] = v;
            }

            if v > fft_state.bar_peak[i] {
                fft_state.bar_peak[i] =
                    fft_state.bar_peak[i] + (v - fft_state.bar_peak[i]) * viz.peak_attack;
            } else {
                fft_state.bar_peak[i] *= viz.peak_release;
                if fft_state.bar_peak[i] < 1e-12 {
                    fft_state.bar_peak[i] = 1e-12;
                }
            }

            let mut x = v / (fft_state.bar_peak[i] + 1e-12);
            x = (x * viz.sensitivity).clamp(0.0, 1.0);
            x = x.powf(viz.curve);

            fft_state.bars[i] = x;
        }

        for i in 0..fft_state.bars.len() {
            fft_state.bars_smooth[i] = fft_state.bars_smooth[i] * 0.98 + fft_state.bars[i] * 0.02;
        }

        let hop = HOP.min(fft_state.mono_ring.len());
        fft_state.mono_ring.drain(0..hop);
    }
}

pub(super) fn reset_fft_state(
    mono_ring: &mut Vec<f32>,
    bars_smooth: &mut [f32],
    bar_peak: &mut [f32],
    spectrum_bits: &Arc<Vec<AtomicU32>>,
) {
    mono_ring.clear();
    bars_smooth.fill(0.0);
    bar_peak.fill(0.0);
    clear_spectrum(spectrum_bits);
}

fn hann_window(n: usize) -> Vec<f32> {
    // Hann: 0.5 - 0.5*cos(2πk/(n-1))
    let denom = (n.saturating_sub(1)).max(1) as f32;
    (0..n)
        .map(|k| {
            let x = (2.0 * std::f32::consts::PI * k as f32) / denom;
            0.5 - 0.5 * x.cos()
        })
        .collect()
}

fn bins_to_bars(mags: &[f32], sample_rate: u32, bars_out: &mut [f32]) {
    let n_bins = mags.len().max(1);
    let sr = sample_rate as f32;

    let f_min = 60.0_f32;
    let f_max = 12_000.0_f32.min(sr * 0.5);

    let log_min = f_min.ln();
    let log_max = f_max.ln();
    let log_span = (log_max - log_min).max(1e-6);

    for v in bars_out.iter_mut() {
        *v = 0.0;
    }

    // For each bar, average magnitudes of bins in its freq range.
    for i in 0..bars_out.len() {
        let a = i as f32 / bars_out.len() as f32;
        let b = (i + 1) as f32 / bars_out.len() as f32;

        let f0 = (log_min + a * log_span).exp();
        let f1 = (log_min + b * log_span).exp();

        let bin0 = ((f0 / (sr * 0.5)) * (n_bins as f32)) as usize;
        let bin1 = ((f1 / (sr * 0.5)) * (n_bins as f32)) as usize;

        let lo = bin0.clamp(0, n_bins - 1);
        let hi = bin1.clamp(lo + 1, n_bins);

        let mut sum = 0.0f32;
        for &m in &mags[lo..hi] {
            sum += m;
        }
        let avg = sum / ((hi - lo) as f32);

        bars_out[i] = avg;
    }
}

pub(super) fn clear_spectrum(spectrum_bits: &Arc<Vec<AtomicU32>>) {
    for a in spectrum_bits.iter() {
        a.store(0.0f32.to_bits(), Ordering::Relaxed);
    }
}
