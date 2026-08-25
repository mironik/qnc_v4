use std::path::Path;
use std::process::Command;

use qnc_service_contracts::WaveformJobResult;

use crate::FfmpegToolchain;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfmpegWaveformOptions {
    pub toolchain: FfmpegToolchain,
}

impl Default for FfmpegWaveformOptions {
    fn default() -> Self {
        Self {
            toolchain: FfmpegToolchain::default(),
        }
    }
}

pub fn build_waveform_peaks(
    media: &Path,
    sample_rate_hz: u32,
    peak_buckets: usize,
) -> Result<WaveformJobResult, String> {
    build_waveform_peaks_with_options(
        media,
        sample_rate_hz,
        peak_buckets,
        &FfmpegWaveformOptions::default(),
    )
}

pub fn build_waveform_peaks_with_options(
    media: &Path,
    sample_rate_hz: u32,
    peak_buckets: usize,
    options: &FfmpegWaveformOptions,
) -> Result<WaveformJobResult, String> {
    if !media.is_file() {
        return Err(format!("izvor ne postoji: {}", media.display()));
    }
    if sample_rate_hz == 0 {
        return Err("waveform sample rate must be greater than zero".into());
    }
    if peak_buckets == 0 {
        return Err("waveform peak bucket count must be greater than zero".into());
    }

    let ffmpeg = options.toolchain.ffmpeg();
    let a1 = extract_lane_peaks(ffmpeg, media, 0, 0, sample_rate_hz, peak_buckets)?;
    let a2 = extract_lane_peaks(ffmpeg, media, 1, 0, sample_rate_hz, peak_buckets)
        .or_else(|_| extract_lane_peaks(ffmpeg, media, 0, 1, sample_rate_hz, peak_buckets))
        .unwrap_or_default();
    let warning = if a2.is_empty() {
        Some("A2 nije dostupna".into())
    } else {
        None
    };

    Ok(WaveformJobResult {
        a1_peaks: a1,
        a2_peaks: a2,
        warning,
    })
}

fn decode_pcm_mono(
    ffmpeg: &Path,
    media: &Path,
    stream_index: u8,
    channel_index: u8,
    sample_rate_hz: u32,
) -> Result<Vec<f32>, String> {
    let pan = format!("pan=mono|c0=c{channel_index}");
    let map = format!("0:a:{stream_index}");
    let rate = sample_rate_hz.to_string();
    let result = Command::new(ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(media)
        .args([
            "-map", &map, "-af", &pan, "-ac", "1", "-ar", &rate, "-f", "f32le", "-",
        ])
        .output()
        .map_err(|error| format!("waveform ffmpeg: {error}"))?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).trim().to_string());
    }
    let samples = bytes_to_f32_samples(&result.stdout);
    if samples.is_empty() {
        return Err("prazan audio uzorak".into());
    }
    Ok(samples)
}

fn bytes_to_f32_samples(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .filter(|sample| sample.is_finite())
        .collect()
}

fn bucket_max_peaks(samples: &[f32], buckets: usize) -> Vec<f32> {
    if samples.is_empty() || buckets == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(buckets);
    let samples_per_bucket = (samples.len() as f64 / buckets as f64).max(1.0);
    for i in 0..buckets {
        let start = ((i as f64) * samples_per_bucket) as usize;
        let end = (((i + 1) as f64) * samples_per_bucket) as usize;
        let end = end.min(samples.len()).max(start.saturating_add(1));
        let peak = samples[start..end]
            .iter()
            .map(|sample| sample.abs())
            .fold(0.0f32, f32::max);
        out.push(peak);
    }
    let max = out.iter().copied().fold(0.0f32, f32::max);
    if max > 0.0 {
        for value in &mut out {
            *value /= max;
        }
    }
    out
}

fn extract_lane_peaks(
    ffmpeg: &Path,
    media: &Path,
    stream_index: u8,
    channel_index: u8,
    sample_rate_hz: u32,
    peak_buckets: usize,
) -> Result<Vec<f32>, String> {
    let samples = decode_pcm_mono(ffmpeg, media, stream_index, channel_index, sample_rate_hz)?;
    Ok(bucket_max_peaks(&samples, peak_buckets))
}

#[cfg(test)]
mod tests {
    use super::{bucket_max_peaks, bytes_to_f32_samples};

    #[test]
    fn bucket_max_peaks_normalizes() {
        let samples = vec![0.0f32, 0.5, -1.0, 0.25, 0.75];
        let peaks = bucket_max_peaks(&samples, 3);
        assert_eq!(peaks.len(), 3);
        assert!((peaks.iter().copied().fold(0.0f32, f32::max) - 1.0).abs() < 0.001);
    }

    #[test]
    fn bytes_to_f32_samples_discards_invalid_samples() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&0.5f32.to_le_bytes());
        raw.extend_from_slice(&f32::NAN.to_le_bytes());
        raw.extend_from_slice(&(-1.0f32).to_le_bytes());

        let samples = bytes_to_f32_samples(&raw);
        assert_eq!(samples, vec![0.5, -1.0]);
    }
}
