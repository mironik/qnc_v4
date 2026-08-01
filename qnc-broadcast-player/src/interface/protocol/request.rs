use serde::{Deserialize, Serialize};

use crate::model::{AudioRuntime, FrameNumber, FrameRange, SourceRuntime};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BroadcastPlaybackRequest {
    pub request_id: String,
    pub source_runtime: SourceRuntime,
    pub execution_range: FrameRange,
    pub initial_frame: FrameNumber,
    pub rate_num: i32,
    pub rate_den: u32,
    pub audio_runtime: AudioRuntime,
}

impl BroadcastPlaybackRequest {
    pub fn new(
        request_id: impl Into<String>,
        source_runtime: SourceRuntime,
    ) -> Result<Self, String> {
        let execution_range = FrameRange::new(0, source_runtime.duration_frames)?;
        let request = Self {
            request_id: request_id.into(),
            source_runtime,
            initial_frame: execution_range.start_frame,
            execution_range,
            rate_num: 1,
            rate_den: 1,
            audio_runtime: AudioRuntime::default(),
        };
        request.validate()?;
        Ok(request)
    }

    pub fn with_range(mut self, execution_range: FrameRange) -> Result<Self, String> {
        self.execution_range = execution_range;
        self.initial_frame = execution_range.start_frame;
        self.validate()?;
        Ok(self)
    }

    pub fn with_initial_frame(mut self, initial_frame: FrameNumber) -> Result<Self, String> {
        self.initial_frame = initial_frame;
        self.validate()?;
        Ok(self)
    }

    pub fn with_rate(mut self, rate_num: i32, rate_den: u32) -> Result<Self, String> {
        self.rate_num = rate_num;
        self.rate_den = rate_den;
        self.validate()?;
        Ok(self)
    }

    pub fn with_audio_runtime(mut self, audio_runtime: AudioRuntime) -> Result<Self, String> {
        self.audio_runtime = audio_runtime;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.request_id.trim().is_empty() {
            return Err("request_id must not be blank".to_string());
        }
        if self.source_runtime.source_id.trim().is_empty() {
            return Err("source_id must not be blank".to_string());
        }
        if self.rate_den == 0 {
            return Err("rate_den must be greater than zero".to_string());
        }
        if self.rate_num <= 0 {
            return Err("rate_num must be greater than zero".to_string());
        }
        if self.execution_range.end_frame > self.source_runtime.duration_frames {
            return Err(format!(
                "execution range end {} is outside source duration {}",
                self.execution_range.end_frame, self.source_runtime.duration_frames
            ));
        }
        if !self.execution_range.contains_position(self.initial_frame) {
            return Err(format!(
                "initial frame {} is outside execution range {}..{}",
                self.initial_frame,
                self.execution_range.start_frame,
                self.execution_range.end_frame
            ));
        }
        Ok(())
    }
}
