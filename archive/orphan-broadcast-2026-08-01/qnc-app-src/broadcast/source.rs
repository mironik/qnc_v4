//! Broadcast playback source construction.
//!
//! `BroadcastPlaybackSource` must be created from media/DB truth. The source
//! FPS is the probed asset `Timebase`; IN/OUT seconds are only a compatibility
//! adapter and are immediately snapped to source frame numbers.

use super::asset::BroadcastMediaAsset;
use super::timebase::{FrameNumber, FrameRange, Timebase};
use super::BroadcastPlaybackSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastSourceBuildError {
    pub message: String,
}

impl BroadcastSourceBuildError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BroadcastSourceRangeSpec {
    Frames(FrameRange),
    Seconds { in_sec: f64, out_sec: f64 },
}

impl BroadcastSourceRangeSpec {
    pub fn to_frame_range(
        self,
        timebase: Timebase,
    ) -> Result<FrameRange, BroadcastSourceBuildError> {
        match self {
            Self::Frames(range) => Ok(range),
            Self::Seconds { in_sec, out_sec } => {
                if !in_sec.is_finite() || !out_sec.is_finite() {
                    return Err(BroadcastSourceBuildError::new(
                        "source IN/OUT seconds must be finite",
                    ));
                }
                if out_sec <= in_sec {
                    return Err(BroadcastSourceBuildError::new(
                        "source OUT must be greater than IN",
                    ));
                }

                let start = timebase.frame_at_seconds(in_sec.max(0.0));
                let end = timebase.frame_at_seconds(out_sec.max(0.0));
                Ok(FrameRange::new(start, end))
            }
        }
    }
}

impl BroadcastPlaybackSource {
    pub fn from_media_asset(
        asset: &BroadcastMediaAsset,
        source_range: BroadcastSourceRangeSpec,
    ) -> Result<Self, BroadcastSourceBuildError> {
        let range = source_range.to_frame_range(asset.source_timebase)?;
        let has_audio = asset.has_audio && asset.audio_channels > 0;
        if !asset.has_video && !has_audio {
            return Err(BroadcastSourceBuildError::new(
                "media asset has neither video nor audio",
            ));
        }

        Ok(Self {
            project_id: asset.project_id.clone(),
            virtual_shot_id: asset.virtual_shot_id.clone(),
            clip_id: asset.clip_id.clone(),
            source_range: range,
            source_timebase: asset.source_timebase,
            has_video: asset.has_video,
            has_audio,
            audio_channels: if has_audio {
                asset.audio_channels.clamp(1, 4)
            } else {
                0
            },
        })
    }
}

pub fn source_range_from_seconds(
    timebase: Timebase,
    in_sec: f64,
    out_sec: f64,
) -> Result<FrameRange, BroadcastSourceBuildError> {
    BroadcastSourceRangeSpec::Seconds { in_sec, out_sec }.to_frame_range(timebase)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::asset::BroadcastMediaAsset;
    use crate::broadcast::probe::BroadcastMediaProbeReport;

    fn asset_from_probe(source_timebase: Timebase, audio_channels: u8) -> BroadcastMediaAsset {
        BroadcastMediaAsset::proxy_local(
            "project",
            "shot",
            "clip",
            "media/proxy.mxf",
            Timebase::from_source_rate(1, 1).unwrap(),
            false,
            false,
        )
        .with_probe_report(BroadcastMediaProbeReport {
            source_timebase,
            has_video: true,
            has_audio: audio_channels > 0,
            audio_channels,
            audio_stream_count: if audio_channels > 0 { 1 } else { 0 },
            video_width: Some(1920),
            video_height: Some(1080),
        })
    }

    #[test]
    fn playback_source_uses_probed_source_timebase_for_second_ranges() {
        let source_timebase = Timebase::from_source_rate(50, 1).unwrap();
        let asset = asset_from_probe(source_timebase, 2);

        let source = BroadcastPlaybackSource::from_media_asset(
            &asset,
            BroadcastSourceRangeSpec::Seconds {
                in_sec: 2.0,
                out_sec: 2.5,
            },
        )
        .unwrap();

        assert_eq!(source.source_timebase, source_timebase);
        assert_eq!(
            source.source_range,
            FrameRange::new(FrameNumber(100), FrameNumber(125))
        );
        assert_eq!(source.audio_channels, 2);
    }

    #[test]
    fn playback_source_accepts_db_frame_range_without_resnapping() {
        let asset = asset_from_probe(Timebase::from_source_rate(30_000, 1_001).unwrap(), 1);
        let range = FrameRange::new(FrameNumber(1001), FrameNumber(2002));

        let source = BroadcastPlaybackSource::from_media_asset(
            &asset,
            BroadcastSourceRangeSpec::Frames(range),
        )
        .unwrap();

        assert_eq!(source.source_range, range);
    }

    #[test]
    fn playback_source_rejects_invalid_second_range() {
        let asset = asset_from_probe(Timebase::from_source_rate(25, 1).unwrap(), 1);

        let err = BroadcastPlaybackSource::from_media_asset(
            &asset,
            BroadcastSourceRangeSpec::Seconds {
                in_sec: 5.0,
                out_sec: 4.0,
            },
        )
        .unwrap_err();

        assert!(err.message.contains("OUT"));
    }

    #[test]
    fn playback_source_disables_audio_when_probe_has_zero_channels() {
        let asset = asset_from_probe(Timebase::from_source_rate(25, 1).unwrap(), 0);

        let source = BroadcastPlaybackSource::from_media_asset(
            &asset,
            BroadcastSourceRangeSpec::Frames(FrameRange::new(FrameNumber(0), FrameNumber(25))),
        )
        .unwrap();

        assert!(source.has_video);
        assert!(!source.has_audio);
        assert_eq!(source.audio_channels, 0);
    }
}
