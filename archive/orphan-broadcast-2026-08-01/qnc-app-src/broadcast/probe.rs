//! Source media probing for broadcast metadata.
//!
//! FPS/timebase must come from the source media, not from UI defaults or test
//! fixtures. This module parses ffprobe output into a rational `Timebase` and
//! basic media capabilities used by asset resolution and playback setup.

use std::process::Command;

use serde_json::Value;

use super::asset::{BroadcastMediaAsset, BroadcastMediaAssetSeed, BroadcastMediaLocation};
use super::layers::MAX_AUDIO_CHANNELS;
use super::timebase::{Timebase, TimebaseParseError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastMediaProbeReport {
    pub source_timebase: Timebase,
    pub has_video: bool,
    pub has_audio: bool,
    pub audio_channels: u8,
    /// Number of audio streams in the container (1 = interleaved multi-channel).
    pub audio_stream_count: u8,
    pub video_width: Option<u32>,
    pub video_height: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaProbeError {
    pub message: String,
}

impl MediaProbeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<TimebaseParseError> for MediaProbeError {
    fn from(value: TimebaseParseError) -> Self {
        Self::new(value.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfprobeCommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfprobeMediaProbe {
    pub program: String,
}

impl Default for FfprobeMediaProbe {
    fn default() -> Self {
        Self {
            program: "ffprobe".into(),
        }
    }
}

impl FfprobeMediaProbe {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }

    pub fn command_for_location(&self, location: &BroadcastMediaLocation) -> FfprobeCommandSpec {
        FfprobeCommandSpec {
            program: self.program.clone(),
            args: vec![
                "-v".into(),
                "error".into(),
                "-show_entries".into(),
                "stream=codec_type,avg_frame_rate,r_frame_rate,channels,width,height".into(),
                "-of".into(),
                "json".into(),
                media_input(location),
            ],
        }
    }

    pub fn probe_asset(
        &self,
        asset: BroadcastMediaAsset,
    ) -> Result<BroadcastMediaAsset, MediaProbeError> {
        let command = self.command_for_location(&asset.location);
        let output = Command::new(&command.program)
            .args(&command.args)
            .output()
            .map_err(|err| MediaProbeError::new(format!("ffprobe failed to start: {err}")))?;

        if !output.status.success() {
            return Err(MediaProbeError::new(format!(
                "ffprobe failed ({})",
                output.status
            )));
        }

        Ok(asset.with_probe_report(parse_ffprobe_json(&output.stdout)?))
    }

    pub fn probe_asset_seed(
        &self,
        seed: BroadcastMediaAssetSeed,
    ) -> Result<BroadcastMediaAsset, MediaProbeError> {
        let command = self.command_for_location(&seed.location);
        let output = Command::new(&command.program)
            .args(&command.args)
            .output()
            .map_err(|err| MediaProbeError::new(format!("ffprobe failed to start: {err}")))?;

        if !output.status.success() {
            return Err(MediaProbeError::new(format!(
                "ffprobe failed ({})",
                output.status
            )));
        }

        Ok(seed.with_probe_report(parse_ffprobe_json(&output.stdout)?))
    }
}

pub fn parse_ffprobe_json(bytes: &[u8]) -> Result<BroadcastMediaProbeReport, MediaProbeError> {
    let root: Value = serde_json::from_slice(bytes)
        .map_err(|err| MediaProbeError::new(format!("invalid ffprobe json: {err}")))?;
    let streams = root
        .get("streams")
        .and_then(Value::as_array)
        .ok_or_else(|| MediaProbeError::new("ffprobe json missing streams"))?;

    let video = streams
        .iter()
        .find(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("video"));
    let audio_streams = streams
        .iter()
        .filter(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("audio"))
        .collect::<Vec<_>>();

    let Some(video) = video else {
        return Err(MediaProbeError::new(
            "source media has no video stream for source timebase",
        ));
    };

    let rate = rate_from_stream(video)?;
    let source_timebase = Timebase::parse_ffprobe_rate(rate)?;
    let per_stream_channels = audio_streams
        .iter()
        .filter_map(|stream| stream.get("channels").and_then(Value::as_u64))
        .map(|channels| (channels as u8).max(1))
        .collect::<Vec<_>>();
    let audio_stream_count = per_stream_channels.len().min(MAX_AUDIO_CHANNELS as usize) as u8;
    let audio_channels = per_stream_channels
        .iter()
        .map(|&c| c as u16)
        .sum::<u16>()
        .min(MAX_AUDIO_CHANNELS as u16) as u8;

    Ok(BroadcastMediaProbeReport {
        source_timebase,
        has_video: true,
        has_audio: audio_channels > 0,
        audio_channels,
        audio_stream_count,
        video_width: video
            .get("width")
            .and_then(Value::as_u64)
            .map(|width| width as u32),
        video_height: video
            .get("height")
            .and_then(Value::as_u64)
            .map(|height| height as u32),
    })
}

fn rate_from_stream(stream: &Value) -> Result<&str, MediaProbeError> {
    let avg = stream.get("avg_frame_rate").and_then(Value::as_str);
    if let Some(rate) = avg.filter(|rate| usable_rate(rate)) {
        return Ok(rate);
    }

    let raw = stream.get("r_frame_rate").and_then(Value::as_str);
    raw.filter(|rate| usable_rate(rate))
        .ok_or_else(|| MediaProbeError::new("video stream has no usable source frame rate"))
}

fn usable_rate(rate: &str) -> bool {
    Timebase::parse_ffprobe_rate(rate).is_ok()
}

fn media_input(location: &BroadcastMediaLocation) -> String {
    match location {
        BroadcastMediaLocation::LocalPath(path) => path.to_string_lossy().into_owned(),
        BroadcastMediaLocation::Url(url) => url.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::broadcast::asset::BroadcastMediaAsset;

    #[test]
    fn parses_ffprobe_json_into_source_timebase_and_channels() {
        let json = br#"{
            "streams": [
                {
                    "codec_type": "video",
                    "avg_frame_rate": "30000/1001",
                    "r_frame_rate": "30000/1001",
                    "width": 1920,
                    "height": 1080
                },
                { "codec_type": "audio", "channels": 4 }
            ]
        }"#;

        let report = parse_ffprobe_json(json).unwrap();

        assert_eq!(
            report.source_timebase,
            Timebase::from_source_rate(30_000, 1_001).unwrap()
        );
        assert_eq!(report.audio_channels, 4);
        assert_eq!(report.audio_stream_count, 1);
        assert_eq!(report.video_width, Some(1920));
        assert_eq!(report.video_height, Some(1080));
    }

    #[test]
    fn sums_channels_across_discrete_mono_audio_streams() {
        let json = br#"{
            "streams": [
                {
                    "codec_type": "video",
                    "avg_frame_rate": "25/1",
                    "r_frame_rate": "25/1"
                },
                { "codec_type": "audio", "channels": 1 },
                { "codec_type": "audio", "channels": 1 },
                { "codec_type": "audio", "channels": 1 },
                { "codec_type": "audio", "channels": 1 }
            ]
        }"#;

        let report = parse_ffprobe_json(json).unwrap();

        assert_eq!(report.audio_channels, 4);
        assert_eq!(report.audio_stream_count, 4);
    }

    #[test]
    fn falls_back_from_zero_avg_rate_to_raw_rate() {
        let json = br#"{
            "streams": [
                {
                    "codec_type": "video",
                    "avg_frame_rate": "0/0",
                    "r_frame_rate": "50/1"
                }
            ]
        }"#;

        let report = parse_ffprobe_json(json).unwrap();

        assert_eq!(
            report.source_timebase,
            Timebase::from_source_rate(50, 1).unwrap()
        );
        assert!(!report.has_audio);
        assert_eq!(report.audio_channels, 0);
    }

    #[test]
    fn rejects_probe_without_video_timebase() {
        let json = br#"{ "streams": [{ "codec_type": "audio", "channels": 2 }] }"#;

        let err = parse_ffprobe_json(json).unwrap_err();

        assert!(err.message.contains("no video stream"));
    }

    #[test]
    fn builds_ffprobe_command_for_local_asset() {
        let probe = FfprobeMediaProbe::new("ffprobe");
        let asset = BroadcastMediaAsset::proxy_local(
            "project",
            "shot",
            "clip",
            PathBuf::from("media/source.mxf"),
            Timebase::from_source_rate(1, 1).unwrap(),
            true,
            true,
        );

        let command = probe.command_for_location(&asset.location);

        assert_eq!(command.program, "ffprobe");
        assert!(command.args.iter().any(|arg| arg == "json"));
        assert_eq!(command.args.last().unwrap(), "media/source.mxf");
    }

    #[test]
    fn asset_can_be_updated_from_probe_report() {
        let asset = BroadcastMediaAsset::proxy_local(
            "project",
            "shot",
            "clip",
            "media/source.mxf",
            Timebase::from_source_rate(1, 1).unwrap(),
            false,
            false,
        );
        let report = BroadcastMediaProbeReport {
            source_timebase: Timebase::from_source_rate(25, 1).unwrap(),
            has_video: true,
            has_audio: true,
            audio_channels: 2,
            audio_stream_count: 1,
            video_width: Some(1920),
            video_height: Some(1080),
        };

        let asset = asset.with_probe_report(report);

        assert_eq!(
            asset.source_timebase,
            Timebase::from_source_rate(25, 1).unwrap()
        );
        assert!(asset.has_video);
        assert!(asset.has_audio);
        assert_eq!(asset.audio_channels, 2);
    }
}
