use serde::{Deserialize, Serialize};

pub type FrameNumber = u64;
pub type FrameDelta = i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timebase {
    pub frame_rate_num: u32,
    pub frame_rate_den: u32,
}

impl Timebase {
    pub fn new(frame_rate_num: u32, frame_rate_den: u32) -> Result<Self, String> {
        if frame_rate_num == 0 || frame_rate_den == 0 {
            return Err("timebase values must be greater than zero".to_string());
        }
        Ok(Self {
            frame_rate_num,
            frame_rate_den,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameRange {
    pub start_frame: FrameNumber,
    pub end_frame: FrameNumber,
}

impl FrameRange {
    pub fn new(start_frame: FrameNumber, end_frame: FrameNumber) -> Result<Self, String> {
        if end_frame <= start_frame {
            return Err("range end_frame must be greater than start_frame".to_string());
        }
        Ok(Self {
            start_frame,
            end_frame,
        })
    }

    pub fn contains_position(self, frame: FrameNumber) -> bool {
        frame >= self.start_frame && frame <= self.end_frame
    }

    pub fn contains_item(self, start_frame: FrameNumber, duration_frames: FrameNumber) -> bool {
        duration_frames > 0
            && start_frame >= self.start_frame
            && start_frame.saturating_add(duration_frames) <= self.end_frame
    }

    pub fn duration_frames(self) -> FrameNumber {
        self.end_frame - self.start_frame
    }
}
