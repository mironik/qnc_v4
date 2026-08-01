//! Broadcast-quality contracts: shared timecode carrier, dual-mono A1–A4, play start.
//!
//! The celluloid/timecode strip is the **common carrier** (zajednička nosilja).
//! Video, audio buses, markers, and effects register against it — none owns time.

use super::av_sync::{
    audio_emit_range, av_queues_in_lockstep, decode_frontier_for_stall, play_start_ready,
    PLAY_START_MIN_BUFFER_FRAMES, STALL_RESUME_BUFFER_FRAMES,
};
use super::layers::AudioChannel;
use super::timebase::{FrameNumber, FrameRange, Timebase};
use super::{BroadcastPlaybackSource, BroadcastSourceKind};

/// Fixture source on a labelled carrier identity.
pub fn quality_source(
    kind: BroadcastSourceKind,
    fps: f64,
    start: i64,
    end: i64,
) -> BroadcastPlaybackSource {
    crate::broadcast::fixture_source(kind, fps, start, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::asset::{
        BroadcastMediaAsset, InMemoryMediaResolver, NullResolvedBroadcastBackend,
    };
    use crate::broadcast::clock::ClockReference;
    use crate::broadcast::payload::BroadcastAudioPayload;
    use crate::broadcast::player::BroadcastPlaybackPump;
    use crate::broadcast::present::mix_audio_buses;
    use crate::broadcast::sync::{AudioSampleSpan, BROADCAST_AUDIO_SAMPLE_RATE_HZ};
    use crate::broadcast::{
        AudioMix, BroadcastMediaKind, BroadcastMediaLocation, BroadcastProgramGraph,
        DecodedAudioBus, UniversalTimelineSpec,
    };
    use std::time::{Duration, Instant};

    fn asset(kind: BroadcastSourceKind) -> BroadcastMediaAsset {
        let source = quality_source(kind, 25.0, 100, 200);
        let (has_video, has_audio, buses) = match kind {
            BroadcastSourceKind::VideoAndAudio => (true, true, 2u8),
            BroadcastSourceKind::VideoOnly => (true, false, 0),
            BroadcastSourceKind::AudioOnly => (false, true, 2),
        };
        BroadcastMediaAsset::from_parts(
            source.project_id,
            source.virtual_shot_id,
            source.clip_id,
            BroadcastMediaKind::Proxy,
            BroadcastMediaLocation::LocalPath(format!("media/{kind:?}.mxf").into()),
            Timebase::from_source_fps(25.0),
            has_video,
            has_audio,
            buses,
        )
    }

    fn pump_for(
        kind: BroadcastSourceKind,
    ) -> BroadcastPlaybackPump<NullResolvedBroadcastBackend, InMemoryMediaResolver> {
        let source = quality_source(kind, 25.0, 100, 200);
        let asset = asset(kind);
        let resolver = InMemoryMediaResolver::new().with_asset(asset);
        BroadcastPlaybackPump::new(
            source,
            NullResolvedBroadcastBackend,
            resolver,
            24,
            8,
            ClockReference::InternalMonotonic,
        )
    }

    #[test]
    fn timecode_carrier_is_shared_nosilja_for_all_layers() {
        let source = quality_source(BroadcastSourceKind::VideoAndAudio, 25.0, 100, 200);
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source);
        assert!(graph.carrier.is_transparent_carrier());
        assert_eq!(graph.carrier.timebase, source.source_timebase);
        assert_eq!(graph.carrier.source_range, source.source_range);
        assert_eq!(graph.carrier.virtual_shot_id, source.virtual_shot_id);

        // Clock / schedule identity follows the carrier — not per-layer clocks.
        let session_tb = graph.carrier.timebase;
        let session_range = graph.carrier.source_range;
        for layer in &graph.layers {
            if let Some(range) = layer.frame_range {
                assert!(
                    range.start.0 >= session_range.start.0
                        && range.end_exclusive.0 <= session_range.end_exclusive.0,
                    "layer {} range must sit on carrier",
                    layer.id
                );
            }
            let _ = session_tb; // one timebase for the program
        }

        let audio_channels: Vec<_> = graph
            .layers
            .iter()
            .filter_map(|l| match l.kind {
                crate::broadcast::ProgramLayerKind::Audio(ch) => Some(ch),
                _ => None,
            })
            .collect();
        assert!(audio_channels.contains(&AudioChannel::A1));
        assert!(audio_channels.contains(&AudioChannel::A2));
    }

    #[test]
    fn a1_through_a4_are_dual_mono_on_carrier() {
        // Dual mono monitor: A1/A3 → L, A2/A4 → R — no L=R fold.
        let span = AudioSampleSpan::new(0, 2);
        let levels = [0.10_f32, 0.20, 0.05, 0.15];
        let channels = [
            AudioChannel::A1,
            AudioChannel::A2,
            AudioChannel::A3,
            AudioChannel::A4,
        ];
        let buses: Vec<_> = channels
            .iter()
            .zip(levels.iter())
            .map(|(ch, &lvl)| {
                let payload = BroadcastAudioPayload::new_f32_interleaved(
                    BROADCAST_AUDIO_SAMPLE_RATE_HZ,
                    1,
                    span,
                    vec![lvl, lvl],
                )
                .unwrap();
                DecodedAudioBus {
                    layer_id: format!("a{}", ch.get()),
                    channel: *ch,
                    mix: AudioMix::UNITY,
                    source_frame: FrameNumber(100),
                    pts_sec: 4.0,
                    media_seek_sec: 4.0,
                    sample_rate_hz: BROADCAST_AUDIO_SAMPLE_RATE_HZ,
                    sample_span: span,
                    payload: Some(payload),
                }
            })
            .collect();

        let (_rate, ch, mixed) = mix_audio_buses(&buses).unwrap();
        assert_eq!(ch, 2, "monitor is stereo dual-mono; buses stay mono");
        let left = levels[0] + levels[2];
        let right = levels[1] + levels[3];
        assert!((mixed[0] - left).abs() < 1e-5, "A1+A3 on left");
        assert!((mixed[1] - right).abs() < 1e-5, "A2+A4 on right");
        assert!((mixed[0] - mixed[1]).abs() > 0.05, "L and R must differ");
    }

    #[test]
    fn ffmpeg_bus_pipe_config_is_mono_per_channel() {
        let cfg = crate::broadcast::FfmpegBroadcastConfig::default();
        assert_eq!(
            cfg.audio_channels, 1,
            "each A1–A4 pipe must be mono PCM on the carrier"
        );
    }

    #[test]
    fn play_start_requires_carrier_lockstep_cushion() {
        let start = FrameNumber(100);
        assert!(!play_start_ready(
            start,
            true,
            true,
            Some(FrameNumber(100)),
            Some(FrameNumber(100)),
            PLAY_START_MIN_BUFFER_FRAMES,
        ));
        assert!(!play_start_ready(
            start,
            true,
            true,
            Some(FrameNumber(103)),
            Some(FrameNumber(103)),
            PLAY_START_MIN_BUFFER_FRAMES,
        ));
        assert!(play_start_ready(
            start,
            true,
            true,
            Some(FrameNumber(111)),
            Some(FrameNumber(111)),
            PLAY_START_MIN_BUFFER_FRAMES,
        ));
        // Divergent queues on the same carrier must not be "ready".
        assert!(!play_start_ready(
            start,
            true,
            true,
            Some(FrameNumber(120)),
            Some(FrameNumber(111)),
            PLAY_START_MIN_BUFFER_FRAMES,
        ));
        assert!(!av_queues_in_lockstep(
            true,
            true,
            Some(FrameNumber(120)),
            Some(FrameNumber(111)),
        ));
    }

    #[test]
    fn preroll_then_play_keeps_av_lockstep_on_carrier() {
        let mut pump = pump_for(BroadcastSourceKind::VideoAndAudio);
        let t0 = Instant::now();
        let start = FrameNumber(100);
        pump.pause(t0);
        pump.seek(start, t0);

        let source = pump.source().clone();
        let mut ready = false;
        for i in 0..80 {
            let t = t0 + Duration::from_millis(i * 5);
            let _ = pump.tick(t).unwrap();
            if play_start_ready(
                start,
                source.expects_video_decode(),
                source.expects_media_audio_decode(),
                pump.newest_video_frame(),
                pump.newest_audio_frame(),
                PLAY_START_MIN_BUFFER_FRAMES,
            ) {
                ready = true;
                break;
            }
        }
        assert!(
            ready,
            "preroll must reach {}-frame carrier cushion before play",
            PLAY_START_MIN_BUFFER_FRAMES
        );
        assert_eq!(
            pump.newest_video_frame(),
            pump.newest_audio_frame(),
            "video/audio newest must share carrier identity"
        );

        let play_at = t0 + Duration::from_millis(200);
        pump.play_from_frame(start, play_at);

        let mut last_audio = None::<FrameNumber>;
        let mut stalls = 0_u32;
        let mut emitted = Vec::new();
        for i in 0..25 {
            let now = play_at + Duration::from_millis(i * 40); // ~1 frame @25fps
            let master = pump.current_frame(now);
            let frontier = decode_frontier_for_stall(
                true,
                true,
                pump.newest_video_frame(),
                pump.newest_audio_frame(),
            );
            if let Some(newest) = frontier {
                if !pump.is_clock_stalled() && master.0 > newest.0 {
                    pump.stall_at(newest);
                    stalls += 1;
                }
            }
            if !pump.is_clock_stalled() {
                let batch = audio_emit_range(
                    last_audio,
                    pump.current_frame(now),
                    pump.newest_audio_frame(),
                )
                .unwrap();
                for frame in batch {
                    assert!(
                        pump.take_audio_frame(frame).is_some(),
                        "contiguous PCM missing on carrier frame {}",
                        frame.0
                    );
                    emitted.push(frame.0);
                    last_audio = Some(frame);
                }
            }
            let _ = pump.tick(now).unwrap();
            if pump.is_clock_stalled() {
                let held = pump.current_frame(now);
                if let Some(newest) = decode_frontier_for_stall(
                    true,
                    true,
                    pump.newest_video_frame(),
                    pump.newest_audio_frame(),
                ) {
                    if crate::broadcast::should_resume_after_stall(
                        held,
                        newest,
                        STALL_RESUME_BUFFER_FRAMES,
                    ) {
                        pump.resume_clock(now);
                    }
                }
            }
            assert!(
                av_queues_in_lockstep(
                    true,
                    true,
                    pump.newest_video_frame(),
                    pump.newest_audio_frame(),
                ),
                "tick {i}: A/V left carrier lockstep"
            );
        }

        assert!(
            emitted.len() >= 8,
            "play start must emit dense carrier PCM, got {emitted:?}"
        );
        for w in emitted.windows(2) {
            assert_eq!(
                w[1],
                w[0] + 1,
                "PCM must stay contiguous on carrier: {emitted:?}"
            );
        }
        assert!(
            stalls <= 2,
            "healthy preroll should rarely stall at start, stalls={stalls}"
        );
    }

    #[test]
    fn decode_refill_follows_carrier_frontier_not_video_alone() {
        // If audio lags on the shared carrier, refill must start from min(v,a).
        let newest_video = Some(FrameNumber(110));
        let newest_audio = Some(FrameNumber(100));
        let frontier = decode_frontier_for_stall(true, true, newest_video, newest_audio);
        assert_eq!(frontier, Some(FrameNumber(100)));
        let refill =
            crate::broadcast::decode_newest_for_refill(true, true, newest_video, newest_audio);
        assert_eq!(
            refill, frontier,
            "refill newest must equal stall frontier on the carrier"
        );
    }

    #[test]
    fn universal_timeline_carrier_matches_source_timecode() {
        let source = quality_source(BroadcastSourceKind::VideoAndAudio, 50.0, 0, 100);
        let spec = UniversalTimelineSpec::source_virtual_shot(&source, true);
        assert_eq!(spec.carrier.timebase, source.source_timebase);
        assert_eq!(spec.carrier.source_range, source.source_range);
        assert!(spec.carrier.is_transparent_carrier());
        assert_eq!(spec.audio_tracks.len(), 2);
        assert_eq!(spec.audio_tracks[0].channel, AudioChannel::A1);
        assert_eq!(spec.audio_tracks[1].channel, AudioChannel::A2);
    }
}
