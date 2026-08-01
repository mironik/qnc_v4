//! Live FFmpeg integration — continuous pipe path (not Null backend).
//!
//! Auto-generates a short lavfi fixture when `ffmpeg` is on PATH, unless
//! `QNC_BROADCAST_TEST_MEDIA` points at an existing file.
//! Skips (pass) when ffmpeg is unavailable — does not fail CI without tools.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::asset::{BroadcastMediaAsset, InMemoryMediaResolver};
use super::av_sync::{play_start_ready, PLAY_START_MIN_BUFFER_FRAMES};
use super::clock::ClockReference;
use super::ffmpeg::{FfmpegBroadcastBackend, FfmpegBroadcastConfig};
use super::hwaccel::DecodeHwaccel;
use super::player::BroadcastPlaybackPump;
use super::present::mix_audio_buses;
use super::timebase::{FrameNumber, FrameRange, Timebase};
use super::BroadcastPlaybackSource;

const FIXTURE_FPS: f64 = 25.0;
const FIXTURE_FRAMES: i64 = 50; // 2.0s @ 25fps
const DECODE_TARGET_FRAMES: usize = 16;

fn ffmpeg_program() -> String {
    std::env::var("QNC_FFMPEG")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "ffmpeg".into())
}

fn media_from_env() -> Option<PathBuf> {
    std::env::var_os("QNC_BROADCAST_TEST_MEDIA").and_then(|v| {
        let p = PathBuf::from(v);
        if p.is_file() {
            Some(p)
        } else {
            None
        }
    })
}

fn generate_lavfi_fixture(out: &Path) -> Result<(), String> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let status = Command::new(ffmpeg_program())
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=2:size=320x180:rate=25",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-ac",
            "1",
            "-ar",
            "48000",
            "-shortest",
            out.to_str().ok_or("fixture path utf-8")?,
        ])
        .status()
        .map_err(|e| format!("ffmpeg spawn failed: {e}"))?;
    if !status.success() {
        return Err(format!("ffmpeg fixture exit {status}"));
    }
    if !out.is_file() {
        return Err("fixture file missing after ffmpeg".into());
    }
    Ok(())
}

fn ensure_fixture() -> Option<PathBuf> {
    if let Some(p) = media_from_env() {
        return Some(p);
    }
    let out = std::env::temp_dir().join("qnc_broadcast_live_fixture_2s.mp4");
    match generate_lavfi_fixture(&out) {
        Ok(()) => Some(out),
        Err(err) => {
            eprintln!("live_ffmpeg_integ SKIP: {err}");
            None
        }
    }
}

fn live_source(path: &Path) -> (BroadcastPlaybackSource, BroadcastMediaAsset) {
    let tb = Timebase::from_source_fps(FIXTURE_FPS);
    let source = BroadcastPlaybackSource {
        project_id: "live_integ".into(),
        virtual_shot_id: "shot_live".into(),
        clip_id: "clip_live".into(),
        source_range: FrameRange::new(FrameNumber(0), FrameNumber(FIXTURE_FRAMES)),
        source_timebase: tb,
        has_video: true,
        has_audio: true,
        audio_channels: 1,
    };
    let asset = BroadcastMediaAsset::proxy_local(
        "live_integ",
        "shot_live",
        "clip_live",
        path.to_path_buf(),
        tb,
        true,
        true,
    );
    (source, asset)
}

fn live_config() -> FfmpegBroadcastConfig {
    FfmpegBroadcastConfig {
        program: ffmpeg_program(),
        output_width: 160,
        output_height: 90,
        audio_channels: 1,
        hwaccel: DecodeHwaccel::None,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::asset::BroadcastResolvedDecodeBackend;

    #[test]
    fn continuous_ffmpeg_decodes_n_frames_with_pcm_without_error() {
        let Some(path) = ensure_fixture() else {
            return;
        };
        let (source, asset) = live_source(&path);
        let resolver = InMemoryMediaResolver::new().with_asset(asset);
        let backend = FfmpegBroadcastBackend::new(live_config()).with_continuous(true);
        let mut pump = BroadcastPlaybackPump::new(
            source.clone(),
            backend,
            resolver,
            24,
            8,
            ClockReference::InternalMonotonic,
        );

        let t0 = Instant::now();
        let start = FrameNumber(0);
        pump.pause(t0);
        pump.seek(start, t0);

        let mut ready = false;
        for i in 0..64 {
            let t = t0 + Duration::from_millis(i * 5);
            pump.tick(t)
                .unwrap_or_else(|e| panic!("preroll decode Error (live ffmpeg): {}", e.message));
            if play_start_ready(
                start,
                true,
                true,
                pump.newest_video_frame(),
                pump.newest_audio_frame(),
                PLAY_START_MIN_BUFFER_FRAMES.min(8),
            ) {
                ready = true;
                break;
            }
        }
        assert!(
            ready,
            "live ffmpeg preroll must fill A/V cushion; v={:?} a={:?}",
            pump.newest_video_frame(),
            pump.newest_audio_frame()
        );
        assert_eq!(
            pump.newest_video_frame(),
            pump.newest_audio_frame(),
            "live lockstep on carrier"
        );

        let play_at = Instant::now();
        pump.play_from_frame(start, play_at);

        let mut pcm_frames = 0_usize;
        let mut video_ready = 0_usize;
        let mut last_audio = None::<FrameNumber>;

        for i in 0..40 {
            let now = play_at + Duration::from_millis(i * 40);
            let tick = pump
                .tick(now)
                .unwrap_or_else(|e| panic!("play tick Error (live ffmpeg): {}", e.message));
            video_ready += tick.decoded.video_frames;
            // Contiguous sink path: take exact audio frames like the engine.
            if let Some(newest) = pump.newest_audio_frame() {
                let master = pump.current_frame(now);
                let end = master.0.min(newest.0);
                let begin = last_audio.map(|f| f.0 + 1).unwrap_or(end);
                if begin <= end {
                    for f in begin..=end {
                        let frame = FrameNumber(f);
                        if let Some(queued) = pump.take_audio_frame(frame) {
                            let (_rate, ch, samples) =
                                mix_audio_buses(&queued.payload).expect("mix");
                            assert_eq!(ch, 2, "monitor fold");
                            assert!(!samples.is_empty(), "PCM empty at carrier frame {}", f);
                            // Sine fixture must not be all zeros on A1.
                            let energy: f32 = samples.iter().map(|s| s.abs()).sum();
                            assert!(energy > 0.0, "expected audible PCM at frame {f}");
                            pcm_frames += 1;
                            last_audio = Some(frame);
                        }
                    }
                }
            }
            if pcm_frames >= DECODE_TARGET_FRAMES {
                break;
            }
        }

        assert!(
            pcm_frames >= DECODE_TARGET_FRAMES,
            "expected ≥{DECODE_TARGET_FRAMES} PCM carrier frames from continuous ffmpeg, got {pcm_frames} (video_decoded={video_ready})"
        );
        assert!(
            pump.newest_video_frame().is_some(),
            "video frontier must advance"
        );
    }

    #[test]
    fn continuous_ffmpeg_sequential_decode_stays_on_carrier() {
        let Some(path) = ensure_fixture() else {
            return;
        };
        let (source, asset) = live_source(&path);
        let resolver = InMemoryMediaResolver::new().with_asset(asset);
        let mut backend = FfmpegBroadcastBackend::new(live_config()).with_continuous(true);

        // Direct backend: decode consecutive resolved plans without engine.
        let graph = crate::broadcast::BroadcastProgramGraph::from_source_virtual_shot(&source);
        let render = crate::broadcast::BroadcastRenderPlan::from_graph(&graph);
        let scheduler = crate::broadcast::BroadcastFrameScheduler::new(render.clone());

        let mut prev = None::<i64>;
        for offset in 0..12 {
            let frame = FrameNumber(offset);
            let plan = crate::broadcast::FrameDecodePlan::from_scheduled(
                &render,
                scheduler.schedule_frame(frame),
            );
            let resolved =
                crate::broadcast::asset::ResolvedFrameDecodePlan::resolve(&plan, &resolver)
                    .expect("resolve");
            let decoded = backend
                .decode_resolved_frame(&resolved)
                .unwrap_or_else(|e| panic!("frame {offset}: {}", e.message));
            assert_eq!(decoded.source_frame, frame);
            assert!(!decoded.video.is_empty(), "video layer missing @ {offset}");
            assert!(!decoded.audio.is_empty(), "audio buses missing @ {offset}");
            assert!(
                decoded.video.iter().any(|v| v.payload.is_some()),
                "video payload None @ {offset}"
            );
            assert!(
                decoded.audio.iter().any(|a| a.payload.is_some()),
                "audio payload None @ {offset}"
            );
            if let Some(p) = prev {
                assert_eq!(offset, p + 1, "must stay sequential on carrier");
            }
            prev = Some(offset);
        }
    }
}
