use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{AudioFormat, FrameNumber, Timebase, VideoFormat};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRuntime {
    pub source_id: String,
    pub duration_frames: FrameNumber,
    pub timebase: Timebase,
    pub source_start_tc: Option<String>,
    pub video_format: Option<VideoFormat>,
    pub audio_format: Option<AudioFormat>,
}

impl SourceRuntime {
    pub fn new(
        source_id: impl Into<String>,
        duration_frames: FrameNumber,
        timebase: Timebase,
    ) -> Result<Self, String> {
        if duration_frames == 0 {
            return Err("source duration_frames must be greater than zero".to_string());
        }
        Ok(Self {
            source_id: source_id.into(),
            duration_frames,
            timebase,
            source_start_tc: None,
            video_format: None,
            audio_format: None,
        })
    }

    pub fn with_video_format(mut self, video_format: VideoFormat) -> Self {
        self.video_format = Some(video_format);
        self
    }

    pub fn with_audio_format(mut self, audio_format: AudioFormat) -> Self {
        self.audio_format = Some(audio_format);
        self
    }
}

pub type SourceMap = BTreeMap<String, SourceRuntime>;
pub type SourceSet = BTreeSet<String>;
