use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldMode {
    Progressive,
    InterlacedUpperFirst,
    InterlacedLowerFirst,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorSpace {
    Rec709,
    Rec2020,
    Srgb,
    Custom(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelAspect {
    pub num: u32,
    pub den: u32,
}

impl PixelAspect {
    pub fn new(num: u32, den: u32) -> Result<Self, String> {
        if num == 0 || den == 0 {
            return Err("pixel aspect values must be greater than zero".to_string());
        }
        Ok(Self { num, den })
    }

    pub fn square() -> Self {
        Self { num: 1, den: 1 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoFormat {
    pub width: u32,
    pub height: u32,
    pub field_mode: FieldMode,
    pub color_space: ColorSpace,
    pub pixel_aspect: PixelAspect,
}

impl VideoFormat {
    pub fn new(
        width: u32,
        height: u32,
        field_mode: FieldMode,
        color_space: ColorSpace,
    ) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("video format width and height must be greater than zero".to_string());
        }
        Ok(Self {
            width,
            height,
            field_mode,
            color_space,
            pixel_aspect: PixelAspect::square(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFormat {
    pub sample_rate_hz: u32,
    pub channel_count: u16,
}

impl AudioFormat {
    pub fn new(sample_rate_hz: u32, channel_count: u16) -> Result<Self, String> {
        if sample_rate_hz == 0 || channel_count == 0 {
            return Err(
                "audio sample rate and channel count must be greater than zero".to_string(),
            );
        }
        Ok(Self {
            sample_rate_hz,
            channel_count,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioRuntime {
    pub enabled_tracks: BTreeMap<String, bool>,
    pub channel_map_id: Option<String>,
    pub monitor_volume_millibels: i32,
    pub muted: bool,
    pub solo_track_id: Option<String>,
}
