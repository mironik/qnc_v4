use std::path::{Path, PathBuf};
use std::process::Command;

use async_trait::async_trait;
use qnc_service_contracts::{
    ArtifactRef, AudioProbe, AudioProbeRequest, AudioWrapRequest, ExtractRangeRequest,
    FilmstripFrameArtifact, FilmstripRequest, FrameExtractRequest, FrameTimebase, MediaLocator,
    MediaProbe, MediaProcessor, MediaRef, PosterExtractRequest, ProxyBuildRequest, ScanMode,
    ServiceError, ServiceResult, WaveformPeaks, WaveformRequest,
};

use crate::ingest::{audio_wrap, proxy_generate, thumb};

#[derive(Debug, Default, Clone)]
pub struct LocalFfmpegMediaProcessor;

impl LocalFfmpegMediaProcessor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl MediaProcessor for LocalFfmpegMediaProcessor {
    async fn probe(&self, input: &MediaRef) -> ServiceResult<MediaProbe> {
        let source = local_media_path(input)?;
        let probed = run_blocking("probe", move || {
            thumb::probe_media(&source).ok_or_else(|| {
                ServiceError::new(
                    "probe_failed",
                    format!("ffprobe could not read media: {}", source.display()),
                )
            })
        })
        .await?;

        map_probe(probed)
    }

    async fn probe_audio(&self, request: AudioProbeRequest) -> ServiceResult<AudioProbe> {
        let source = local_media_path(&request.input)?;
        let probed = run_blocking("probe_audio", move || {
            thumb::probe_media(&source).ok_or_else(|| {
                ServiceError::new(
                    "probe_failed",
                    format!("ffprobe could not read audio media: {}", source.display()),
                )
            })
        })
        .await?;

        Ok(map_audio_probe(probed))
    }

    async fn extract_frame(&self, request: FrameExtractRequest) -> ServiceResult<ArtifactRef> {
        let source = local_media_path(&request.input)?;
        let output_path = request.output_path;
        let frame = request.frame.max(0);

        run_blocking("extract_frame", move || {
            let probe = thumb::probe_media(&source).ok_or_else(|| {
                ServiceError::new(
                    "probe_failed",
                    format!("ffprobe could not read media: {}", source.display()),
                )
            })?;
            let fps = valid_fps(probe.fps)?;
            let seek_sec = frame as f64 / fps;
            thumb::extract_preview_jpeg_at_seek(&source, &output_path, seek_sec)
                .map_err(|message| ServiceError::new("frame_extract_failed", message))?;
            Ok(artifact(output_path))
        })
        .await
    }

    async fn extract_poster(&self, request: PosterExtractRequest) -> ServiceResult<ArtifactRef> {
        let source = local_media_path(&request.input)?;
        let output_path = request.output_path;
        let seek_sec = if request.seek_sec.is_finite() {
            request.seek_sec.max(0.0)
        } else {
            0.0
        };

        run_blocking("extract_poster", move || {
            thumb::extract_poster_jpeg_at_seek(&source, &output_path, seek_sec)
                .map_err(|message| ServiceError::new("poster_extract_failed", message))?;
            Ok(artifact(output_path))
        })
        .await
    }

    async fn build_filmstrip(
        &self,
        request: FilmstripRequest,
    ) -> ServiceResult<Vec<FilmstripFrameArtifact>> {
        if request.frame_count < 2 || request.seek_seconds.len() < 2 {
            return Err(ServiceError::new(
                "invalid_filmstrip_request",
                "Filmstrip requires at least two frames.",
            ));
        }
        if request.frame_count != request.seek_seconds.len() {
            return Err(ServiceError::new(
                "invalid_filmstrip_request",
                "Filmstrip frame count must match explicit seek list.",
            ));
        }

        let source = local_media_path(&request.input)?;
        let output_dir = request.output_dir;
        let seeks = request.seek_seconds;

        run_blocking("build_filmstrip", move || {
            let outputs: Vec<PathBuf> = seeks
                .iter()
                .enumerate()
                .map(|(index, sec)| thumb::filmstrip_frame_path(&output_dir, index, *sec))
                .collect();

            let missing: Vec<usize> = (0..outputs.len())
                .filter(|&index| !file_ready(&outputs[index]))
                .collect();

            if !missing.is_empty() {
                let batch_seeks: Vec<f64> = missing.iter().map(|&index| seeks[index]).collect();
                let batch_outputs: Vec<PathBuf> = missing
                    .iter()
                    .map(|&index| outputs[index].clone())
                    .collect();
                let results =
                    thumb::extract_filmstrip_frames_at_seeks(&source, &batch_seeks, &batch_outputs);
                for (slot, result) in missing.iter().zip(results.into_iter()) {
                    let index = *slot;
                    let sec = seeks[index];
                    let out = &outputs[index];
                    let ok = match result {
                        Ok(()) if file_ready(out) => true,
                        _ => {
                            thumb::extract_poster_jpeg_at_seek_cpu(&source, out, sec).is_ok()
                                && file_ready(out)
                        }
                    };
                    if !ok {
                        tracing::warn!(
                            "filmstrip: frame missing source={} index={} seek={:.2}",
                            source.display(),
                            index,
                            sec
                        );
                    }
                }
            }

            let frames: Vec<FilmstripFrameArtifact> = seeks
                .iter()
                .enumerate()
                .filter_map(|(index, seek_sec)| {
                    let path = outputs[index].clone();
                    file_ready(&path).then(|| FilmstripFrameArtifact {
                        index,
                        seek_sec: *seek_sec,
                        artifact: artifact(path),
                    })
                })
                .collect();

            if frames.is_empty() {
                return Err(ServiceError::new(
                    "filmstrip_failed",
                    "Filmstrip did not produce any valid JPEG frames.",
                ));
            }

            Ok(frames)
        })
        .await
    }

    async fn build_proxy(&self, request: ProxyBuildRequest) -> ServiceResult<ArtifactRef> {
        let source = local_media_path(&request.input)?;
        let output_path = request.output_path;

        run_blocking("build_proxy", move || {
            proxy_generate::generate_field_proxy(&source, &output_path)
                .map_err(|message| ServiceError::new("proxy_failed", message))?;
            Ok(artifact(output_path))
        })
        .await
    }

    async fn build_audio_wrap(&self, request: AudioWrapRequest) -> ServiceResult<ArtifactRef> {
        let source = local_media_path(&request.input)?;
        let output_path = request.output_path;
        let fps = request.fps;

        run_blocking("build_audio_wrap", move || {
            audio_wrap::wrap_audio_with_timecode(&source, &output_path, fps)
                .map_err(|message| ServiceError::new("audio_wrap_failed", message))?;
            Ok(artifact(output_path))
        })
        .await
    }

    async fn build_waveform(&self, request: WaveformRequest) -> ServiceResult<WaveformPeaks> {
        if request.range.is_some() {
            return Err(ServiceError::new(
                "unsupported_waveform_range",
                "Local waveform generation currently supports whole-clip waveforms only.",
            ));
        }
        if request.peak_buckets == 0 {
            return Err(ServiceError::new(
                "invalid_waveform_request",
                "Waveform peak bucket count must be greater than zero.",
            ));
        }
        if request.sample_rate_hz == 0 {
            return Err(ServiceError::new(
                "invalid_waveform_request",
                "Waveform sample rate must be greater than zero.",
            ));
        }

        let source = local_media_path(&request.input)?;
        let peak_buckets = request.peak_buckets;
        let sample_rate_hz = request.sample_rate_hz;

        run_blocking("build_waveform", move || {
            let ffmpeg = thumb::resolve_ffmpeg()
                .ok_or_else(|| ServiceError::new("ffmpeg_unavailable", "ffmpeg nije dostupan"))?;
            let a1 = extract_lane_peaks(&ffmpeg, &source, 0, 0, sample_rate_hz, peak_buckets)
                .map_err(|message| ServiceError::new("waveform_failed", message))?;
            let a2 = extract_lane_peaks(&ffmpeg, &source, 1, 0, sample_rate_hz, peak_buckets)
                .or_else(|_| {
                    extract_lane_peaks(&ffmpeg, &source, 0, 1, sample_rate_hz, peak_buckets)
                })
                .unwrap_or_default();
            let warning = if a2.is_empty() {
                Some("A2 nije dostupna".into())
            } else {
                None
            };

            Ok(WaveformPeaks {
                a1_peaks: a1,
                a2_peaks: a2,
                warning,
            })
        })
        .await
    }

    async fn extract_range(&self, _request: ExtractRangeRequest) -> ServiceResult<ArtifactRef> {
        Err(ServiceError::new(
            "unsupported_operation",
            "Range extraction is not connected to the local FFmpeg adapter yet.",
        ))
    }
}

async fn run_blocking<T, F>(label: &'static str, work: F) -> ServiceResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> ServiceResult<T> + Send + 'static,
{
    tokio::task::spawn_blocking(work).await.map_err(|error| {
        ServiceError::new(
            "worker_join_failed",
            format!("{label} worker failed to join: {error}"),
        )
    })?
}

fn local_media_path(input: &MediaRef) -> ServiceResult<PathBuf> {
    match &input.locator {
        MediaLocator::LocalPath { path } => Ok(path.clone()),
        MediaLocator::IntranetPath { uri } => {
            if uri.starts_with("http://") || uri.starts_with("https://") {
                Err(ServiceError::new(
                    "remote_media_not_local",
                    "Local FFmpeg adapter cannot read HTTP media directly.",
                ))
            } else {
                Ok(PathBuf::from(uri))
            }
        }
        MediaLocator::ManagedAsset { asset_id } => Err(ServiceError::new(
            "managed_asset_not_resolved",
            format!("Managed asset must be resolved before local FFmpeg processing: {asset_id}"),
        )),
    }
}

fn map_probe(probe: thumb::MediaProbe) -> ServiceResult<MediaProbe> {
    let (width, height) = parse_resolution(&probe.resolution);
    let timebase = timebase_from_probe_fps(probe.fps)?;
    let duration_frames = if probe.duration_sec.is_finite() && probe.duration_sec > 0.0 {
        Some((probe.duration_sec * probe.fps).round() as i64)
    } else {
        None
    };

    Ok(MediaProbe {
        width,
        height,
        duration_sec: Some(probe.duration_sec).filter(|value| value.is_finite() && *value > 0.0),
        timebase,
        scan_mode: scan_mode(&probe),
        codec: probe.codec,
        field_order: probe.field_order,
        frame_count: duration_frames,
        duration_frames,
        has_video: width > 0 && height > 0,
        has_audio: probe.has_audio,
        audio_channels: probe.audio_channels as u16,
    })
}

fn map_audio_probe(probe: thumb::MediaProbe) -> AudioProbe {
    AudioProbe {
        duration_sec: Some(probe.duration_sec).filter(|value| value.is_finite() && *value > 0.0),
        codec: probe.codec,
        has_audio: probe.has_audio,
        audio_channels: probe.audio_channels as u16,
    }
}

fn parse_resolution(resolution: &str) -> (u32, u32) {
    let Some((w, h)) = resolution.split_once('x') else {
        return (0, 0);
    };
    let width = w.trim().parse().unwrap_or(0);
    let height = h.trim().parse().unwrap_or(0);
    (width, height)
}

fn scan_mode(probe: &thumb::MediaProbe) -> ScanMode {
    if !probe.interlaced {
        return if probe.field_order == "unknown" {
            ScanMode::Unknown
        } else {
            ScanMode::Progressive
        };
    }

    match probe.field_order.as_str() {
        "tt" | "tb" => ScanMode::InterlacedTopFieldFirst,
        "bb" | "bt" => ScanMode::InterlacedBottomFieldFirst,
        raw if raw.contains("top") => ScanMode::InterlacedTopFieldFirst,
        raw if raw.contains("bottom") => ScanMode::InterlacedBottomFieldFirst,
        _ => ScanMode::Unknown,
    }
}

fn valid_fps(fps: f64) -> ServiceResult<f64> {
    if fps.is_finite() && fps > 0.0 {
        Ok(fps)
    } else {
        Err(ServiceError::new(
            "invalid_probe_fps",
            "Probe did not return a valid positive FPS value.",
        ))
    }
}

fn timebase_from_probe_fps(fps: f64) -> ServiceResult<FrameTimebase> {
    let fps = valid_fps(fps)?;
    let denominator = 1_000_000u64;
    let numerator = (fps * denominator as f64).round() as u64;
    if numerator == 0 || numerator > u32::MAX as u64 {
        return Err(ServiceError::new(
            "invalid_probe_fps",
            format!("Probe FPS is outside supported range: {fps}"),
        ));
    }

    let divisor = gcd(numerator, denominator);
    FrameTimebase::new((numerator / divisor) as u32, (denominator / divisor) as u32)
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let next = a % b;
        a = b;
        b = next;
    }
    a.max(1)
}

fn artifact(path: PathBuf) -> ArtifactRef {
    ArtifactRef {
        media_type: media_type_for_path(&path).to_string(),
        path,
        render_version: None,
    }
}

fn media_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "mp4" => "video/mp4",
        "mxf" => "application/mxf",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

fn file_ready(path: &Path) -> bool {
    path.is_file() && path.metadata().map(|m| m.len()).unwrap_or(0) > 0
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
