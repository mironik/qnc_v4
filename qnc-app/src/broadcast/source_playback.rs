//! Source-first playback contracts.
//!
//! Tests and the player must classify [`BroadcastPlaybackSource`] **before**
//! asserting video/audio queue or mix behaviour.

use super::{BroadcastPlaybackSource, BroadcastSourceKind, FrameNumber, FrameRange, Timebase};

/// Build a labelled fixture source for kind-driven tests.
pub fn fixture_source(
    kind: BroadcastSourceKind,
    fps: f64,
    start: i64,
    end: i64,
) -> BroadcastPlaybackSource {
    let (has_video, has_audio, audio_channels) = match kind {
        BroadcastSourceKind::VideoAndAudio => (true, true, 2),
        BroadcastSourceKind::VideoOnly => (true, false, 0),
        BroadcastSourceKind::AudioOnly => (false, true, 2),
    };
    BroadcastPlaybackSource {
        project_id: format!("proj_{kind:?}"),
        virtual_shot_id: format!("shot_{kind:?}"),
        clip_id: format!("clip_{kind:?}"),
        source_range: FrameRange::new(FrameNumber(start), FrameNumber(end)),
        source_timebase: Timebase::from_source_fps(fps),
        has_video,
        has_audio,
        audio_channels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::asset::{
        BroadcastMediaAsset, InMemoryMediaResolver, NullResolvedBroadcastBackend,
    };
    use crate::broadcast::clock::ClockReference;
    use crate::broadcast::player::BroadcastPlaybackPump;
    use crate::broadcast::session::BroadcastPlaybackSession;
    use crate::broadcast::source::BroadcastSourceRangeSpec;
    use crate::broadcast::{BroadcastMediaKind, BroadcastMediaLocation};
    use std::time::Instant;

    fn asset_for(kind: BroadcastSourceKind) -> BroadcastMediaAsset {
        let (has_video, has_audio, ch) = match kind {
            BroadcastSourceKind::VideoAndAudio => (true, true, 2u8),
            BroadcastSourceKind::VideoOnly => (true, false, 0),
            BroadcastSourceKind::AudioOnly => (false, true, 2),
        };
        BroadcastMediaAsset::from_parts(
            "project",
            "shot",
            "clip",
            BroadcastMediaKind::Proxy,
            BroadcastMediaLocation::LocalPath(format!("media/{kind:?}.mxf").into()),
            Timebase::from_source_fps(25.0),
            has_video,
            has_audio,
            ch,
        )
    }

    #[test]
    fn source_kind_from_asset_is_authoritative() {
        for kind in [
            BroadcastSourceKind::VideoAndAudio,
            BroadcastSourceKind::VideoOnly,
            BroadcastSourceKind::AudioOnly,
        ] {
            let asset = asset_for(kind);
            let source = BroadcastPlaybackSource::from_media_asset(
                &asset,
                BroadcastSourceRangeSpec::Frames(FrameRange::new(FrameNumber(0), FrameNumber(100))),
            )
            .expect("build");
            assert_eq!(source.kind(), kind, "asset → source must preserve {kind:?}");
            assert_eq!(
                source.expects_video_decode(),
                matches!(
                    kind,
                    BroadcastSourceKind::VideoAndAudio | BroadcastSourceKind::VideoOnly
                )
            );
            assert_eq!(
                source.expects_media_audio_decode(),
                matches!(
                    kind,
                    BroadcastSourceKind::VideoAndAudio | BroadcastSourceKind::AudioOnly
                )
            );
        }
    }

    #[test]
    fn fixture_source_kind_matches_construction() {
        for kind in [
            BroadcastSourceKind::VideoAndAudio,
            BroadcastSourceKind::VideoOnly,
            BroadcastSourceKind::AudioOnly,
        ] {
            assert_eq!(fixture_source(kind, 25.0, 100, 200).kind(), kind);
        }
    }

    #[test]
    fn session_queues_follow_source_kind() {
        for kind in [
            BroadcastSourceKind::VideoAndAudio,
            BroadcastSourceKind::VideoOnly,
            BroadcastSourceKind::AudioOnly,
        ] {
            let source = fixture_source(kind, 25.0, 100, 200);
            assert_eq!(source.kind(), kind);

            let mut session: BroadcastPlaybackSession<&'static str, i64> =
                BroadcastPlaybackSession::new(source.clone(), 8, ClockReference::InternalMonotonic);
            assert!(session.source().identity_matches(&source));
            assert_eq!(session.source().kind(), kind);

            session.push_decoded_video_frame(FrameNumber(100), "v");
            session.push_decoded_audio_frame(FrameNumber(100), 100);

            match kind {
                BroadcastSourceKind::VideoAndAudio => {
                    assert_eq!(session.queued_video_len(), 1);
                    assert_eq!(session.queued_audio_len(), 1);
                }
                BroadcastSourceKind::VideoOnly => {
                    assert_eq!(session.queued_video_len(), 1);
                    assert_eq!(
                        session.queued_audio_len(),
                        0,
                        "VideoOnly must not accept media audio pushes"
                    );
                }
                BroadcastSourceKind::AudioOnly => {
                    assert_eq!(
                        session.queued_video_len(),
                        0,
                        "AudioOnly must not accept video pushes"
                    );
                    assert_eq!(session.queued_audio_len(), 1);
                }
            }
        }
    }

    #[test]
    fn pump_decode_fill_follows_source_kind() {
        for kind in [
            BroadcastSourceKind::VideoAndAudio,
            BroadcastSourceKind::VideoOnly,
            BroadcastSourceKind::AudioOnly,
        ] {
            let source = fixture_source(kind, 25.0, 100, 200);
            assert_eq!(source.kind(), kind);

            let asset = BroadcastMediaAsset::from_parts(
                &source.project_id,
                &source.virtual_shot_id,
                &source.clip_id,
                BroadcastMediaKind::Proxy,
                BroadcastMediaLocation::LocalPath("media/x.mxf".into()),
                source.source_timebase,
                source.has_video,
                source.has_audio,
                source.audio_channels,
            );
            let resolver = InMemoryMediaResolver::new().with_asset(asset);
            let mut pump = BroadcastPlaybackPump::new(
                source.clone(),
                NullResolvedBroadcastBackend,
                resolver,
                8,
                3,
                ClockReference::InternalMonotonic,
            );
            assert_eq!(pump.source().kind(), kind);
            assert!(pump.source().identity_matches(&source));

            let t0 = Instant::now();
            pump.play_from_frame(FrameNumber(100), t0);
            let tick = pump.tick(t0).unwrap();

            match kind {
                BroadcastSourceKind::VideoAndAudio => {
                    assert!(
                        tick.decoded.video_frames >= 1 && tick.decoded.audio_frames >= 1,
                        "{kind:?}: {tick:?}"
                    );
                }
                BroadcastSourceKind::VideoOnly => {
                    assert!(tick.decoded.video_frames >= 1, "{kind:?}");
                    assert!(
                        !pump.source().expects_media_audio_decode(),
                        "VideoOnly is not a media-audio source"
                    );
                    assert_eq!(
                        pump.newest_video_frame().is_some(),
                        true,
                        "VideoOnly must fill video frontier"
                    );
                }
                BroadcastSourceKind::AudioOnly => {
                    assert_eq!(tick.decoded.video_frames, 0, "{kind:?}");
                    assert!(tick.decoded.audio_frames >= 1, "{kind:?}");
                    assert!(pump.source().expects_media_audio_decode());
                    assert!(!pump.source().expects_video_decode());
                }
            }
        }
    }

    #[test]
    fn source_identity_mismatch_is_different_clip() {
        let a = fixture_source(BroadcastSourceKind::VideoAndAudio, 25.0, 0, 50);
        let mut b = a.clone();
        b.clip_id = "other_clip".into();
        assert!(!a.identity_matches(&b));
        assert_eq!(a.kind(), b.kind());
    }

    #[test]
    fn mix_and_emit_only_after_source_kind_known() {
        // Audio contiguous contract applies to media-audio kinds; VideoOnly has
        // no media PCM to emit.
        use crate::broadcast::av_sync::simulate_contiguous_emits;

        for kind in [
            BroadcastSourceKind::VideoAndAudio,
            BroadcastSourceKind::AudioOnly,
        ] {
            let source = fixture_source(kind, 25.0, 0, 30);
            assert!(source.expects_media_audio_decode());
            let emitted =
                simulate_contiguous_emits(&[0, 5], &[0, 5]).expect("emit for media-audio source");
            assert_eq!(emitted, (0..=5).collect::<Vec<_>>());
        }

        let video_only = fixture_source(BroadcastSourceKind::VideoOnly, 25.0, 0, 30);
        assert!(!video_only.expects_media_audio_decode());
        assert!(video_only.expects_video_decode());
    }
}
