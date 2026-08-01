//! Host/API adapter for broadcast source construction.
//!
//! Existing Story payloads often expose IN/OUT in seconds and may still carry a
//! legacy float FPS field. This adapter intentionally ignores legacy FPS. It
//! uses Story identity + a probed `BroadcastMediaAsset`; the asset timebase is
//! the only source frame rate used to build `BroadcastPlaybackSource`.

use super::asset::{BroadcastMediaAsset, BroadcastMediaAssetSeed};
use super::source::{BroadcastSourceBuildError, BroadcastSourceRangeSpec};
use super::BroadcastPlaybackSource;

#[derive(Debug, Clone, PartialEq)]
pub struct BroadcastHostSourceRef {
    pub project_id: String,
    pub virtual_shot_id: String,
    pub clip_id: String,
    pub in_seconds: Option<f64>,
    pub out_seconds: Option<f64>,
    pub duration_sec: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastHostSourceError {
    pub message: String,
}

impl BroadcastHostSourceError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<BroadcastSourceBuildError> for BroadcastHostSourceError {
    fn from(value: BroadcastSourceBuildError) -> Self {
        Self::new(value.message)
    }
}

impl BroadcastHostSourceRef {
    pub fn from_story_fields(
        project_id: impl Into<String>,
        shot_id: impl Into<String>,
        root_shot_id: impl Into<String>,
        clip_id: impl Into<String>,
        in_seconds: Option<f64>,
        out_seconds: Option<f64>,
        duration_sec: f64,
    ) -> Result<Self, BroadcastHostSourceError> {
        let project_id = project_id.into();
        let shot_id = shot_id.into();
        let root_shot_id = root_shot_id.into();
        let clip_id = clip_id.into();
        let virtual_shot_id =
            first_non_empty([shot_id.as_str(), root_shot_id.as_str(), clip_id.as_str()])
                .ok_or_else(|| {
                    BroadcastHostSourceError::new("story source is missing virtual shot id")
                })?;

        if project_id.trim().is_empty() {
            return Err(BroadcastHostSourceError::new(
                "story source is missing project id",
            ));
        }
        if clip_id.trim().is_empty() {
            return Err(BroadcastHostSourceError::new(
                "story source is missing clip id",
            ));
        }

        Ok(Self {
            project_id,
            virtual_shot_id: virtual_shot_id.to_string(),
            clip_id,
            in_seconds,
            out_seconds,
            duration_sec,
        })
    }

    pub fn proxy_url_seed(&self, url: impl Into<String>) -> BroadcastMediaAssetSeed {
        BroadcastMediaAssetSeed::proxy_url(
            self.project_id.clone(),
            self.virtual_shot_id.clone(),
            self.clip_id.clone(),
            url,
        )
    }

    pub fn proxy_local_seed(&self, path: impl Into<std::path::PathBuf>) -> BroadcastMediaAssetSeed {
        BroadcastMediaAssetSeed::proxy_local(
            self.project_id.clone(),
            self.virtual_shot_id.clone(),
            self.clip_id.clone(),
            path,
        )
    }

    pub fn range_spec(&self) -> Result<BroadcastSourceRangeSpec, BroadcastHostSourceError> {
        let in_sec = self.in_seconds.unwrap_or(0.0).max(0.0);
        let out_sec = match self.out_seconds {
            Some(out) => out,
            None if self.duration_sec.is_finite() && self.duration_sec > 0.0 => self.duration_sec,
            None => {
                return Err(BroadcastHostSourceError::new(
                    "story source is missing OUT and duration",
                ))
            }
        };

        if !in_sec.is_finite() || !out_sec.is_finite() || out_sec <= in_sec {
            return Err(BroadcastHostSourceError::new(
                "story source has invalid IN/OUT seconds",
            ));
        }

        Ok(BroadcastSourceRangeSpec::Seconds { in_sec, out_sec })
    }

    pub fn playback_source_from_asset(
        &self,
        asset: &BroadcastMediaAsset,
    ) -> Result<BroadcastPlaybackSource, BroadcastHostSourceError> {
        self.ensure_asset_matches(asset)?;
        Ok(BroadcastPlaybackSource::from_media_asset(
            asset,
            self.range_spec()?,
        )?)
    }

    fn ensure_asset_matches(
        &self,
        asset: &BroadcastMediaAsset,
    ) -> Result<(), BroadcastHostSourceError> {
        if asset.project_id != self.project_id
            || asset.virtual_shot_id != self.virtual_shot_id
            || asset.clip_id != self.clip_id
        {
            return Err(BroadcastHostSourceError::new(format!(
                "asset identity {}/{}/{} does not match story source {}/{}/{}",
                asset.project_id,
                asset.virtual_shot_id,
                asset.clip_id,
                self.project_id,
                self.virtual_shot_id,
                self.clip_id
            )));
        }
        Ok(())
    }
}

fn first_non_empty<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    values
        .into_iter()
        .map(str::trim)
        .find(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::probe::BroadcastMediaProbeReport;
    use crate::broadcast::timebase::{FrameNumber, FrameRange, Timebase};

    fn host_source() -> BroadcastHostSourceRef {
        BroadcastHostSourceRef::from_story_fields(
            "project",
            "shot",
            "",
            "clip",
            Some(2.0),
            Some(2.5),
            10.0,
        )
        .unwrap()
    }

    fn probed_asset(source: &BroadcastHostSourceRef, timebase: Timebase) -> BroadcastMediaAsset {
        source
            .proxy_url_seed("http://127.0.0.1/api/story/media")
            .with_probe_report(BroadcastMediaProbeReport {
                source_timebase: timebase,
                has_video: true,
                has_audio: true,
                audio_channels: 2,
                audio_stream_count: 1,
                video_width: Some(1920),
                video_height: Some(1080),
            })
    }

    #[test]
    fn host_source_uses_probed_asset_timebase_for_frame_range() {
        let host_source = host_source();
        let timebase = Timebase::from_source_rate(50, 1).unwrap();
        let asset = probed_asset(&host_source, timebase);

        let source = host_source.playback_source_from_asset(&asset).unwrap();

        assert_eq!(source.source_timebase, timebase);
        assert_eq!(
            source.source_range,
            FrameRange::new(FrameNumber(100), FrameNumber(125))
        );
        assert_eq!(source.audio_channels, 2);
    }

    #[test]
    fn host_source_uses_duration_when_out_is_missing() {
        let host_source = BroadcastHostSourceRef::from_story_fields(
            "project", "shot", "", "clip", None, None, 4.0,
        )
        .unwrap();
        let asset = probed_asset(&host_source, Timebase::from_source_rate(25, 1).unwrap());

        let source = host_source.playback_source_from_asset(&asset).unwrap();

        assert_eq!(
            source.source_range,
            FrameRange::new(FrameNumber(0), FrameNumber(100))
        );
    }

    #[test]
    fn host_source_builds_seed_without_timebase() {
        let host_source = host_source();

        let seed = host_source.proxy_url_seed("http://127.0.0.1/api/story/media");

        assert_eq!(seed.project_id, "project");
        assert_eq!(seed.virtual_shot_id, "shot");
        assert_eq!(seed.clip_id, "clip");
    }

    #[test]
    fn host_source_rejects_asset_identity_mismatch() {
        let host_source = host_source();
        let other_source = BroadcastHostSourceRef::from_story_fields(
            "project", "other", "", "clip", None, None, 1.0,
        )
        .unwrap();
        let asset = probed_asset(&other_source, Timebase::from_source_rate(25, 1).unwrap());

        let err = host_source.playback_source_from_asset(&asset).unwrap_err();

        assert!(err.message.contains("does not match"));
    }

    #[test]
    fn host_source_prefers_root_or_clip_identity_when_shot_id_is_empty() {
        let source = BroadcastHostSourceRef::from_story_fields(
            "project", "", "root", "clip", None, None, 1.0,
        )
        .unwrap();
        assert_eq!(source.virtual_shot_id, "root");

        let source =
            BroadcastHostSourceRef::from_story_fields("project", "", "", "clip", None, None, 1.0)
                .unwrap();
        assert_eq!(source.virtual_shot_id, "clip");
    }
}
