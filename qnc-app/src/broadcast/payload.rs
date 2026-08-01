//! Concrete media payload contracts for native broadcast backends.
//!
//! Decode planning stays backend-neutral, but a native player still needs a
//! strict payload shape once real frames arrive: video frames with explicit
//! pixel layout, and audio blocks as 48 kHz PCM aligned to the carrier-derived
//! sample span.

use super::backend::{AudioDecodeRequest, DecodedProgramFrame};
use super::layers::MAX_AUDIO_CHANNELS;
use super::sync::{AudioSampleSpan, BROADCAST_AUDIO_SAMPLE_RATE_HZ};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPayloadError {
    pub message: String,
}

impl MediaPayloadError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastPixelFormat {
    Rgba8,
    Bgra8,
}

impl BroadcastPixelFormat {
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgba8 | Self::Bgra8 => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastColorSpace {
    Bt709,
    Bt601,
    Srgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastScanMode {
    Progressive,
    InterlacedTopFieldFirst,
    InterlacedBottomFieldFirst,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastVideoPayload {
    pub width: u32,
    pub height: u32,
    pub stride_bytes: usize,
    pub pixel_format: BroadcastPixelFormat,
    pub color_space: BroadcastColorSpace,
    pub scan_mode: BroadcastScanMode,
    pub data: Vec<u8>,
}

impl BroadcastVideoPayload {
    pub fn new_rgba8(width: u32, height: u32, data: Vec<u8>) -> Result<Self, MediaPayloadError> {
        let stride_bytes = checked_stride(width, BroadcastPixelFormat::Rgba8)?;
        Self::with_layout(
            width,
            height,
            stride_bytes,
            BroadcastPixelFormat::Rgba8,
            BroadcastColorSpace::Bt709,
            BroadcastScanMode::Progressive,
            data,
        )
    }

    pub fn with_layout(
        width: u32,
        height: u32,
        stride_bytes: usize,
        pixel_format: BroadcastPixelFormat,
        color_space: BroadcastColorSpace,
        scan_mode: BroadcastScanMode,
        data: Vec<u8>,
    ) -> Result<Self, MediaPayloadError> {
        if width == 0 || height == 0 {
            return Err(MediaPayloadError::new("video dimensions must be non-zero"));
        }

        let min_stride = checked_stride(width, pixel_format)?;
        if stride_bytes < min_stride {
            return Err(MediaPayloadError::new(format!(
                "video stride {stride_bytes} is smaller than minimum {min_stride}"
            )));
        }

        let expected = stride_bytes
            .checked_mul(height as usize)
            .ok_or_else(|| MediaPayloadError::new("video buffer size overflow"))?;
        if data.len() != expected {
            return Err(MediaPayloadError::new(format!(
                "video buffer has {} bytes, expected {expected}",
                data.len()
            )));
        }

        Ok(Self {
            width,
            height,
            stride_bytes,
            pixel_format,
            color_space,
            scan_mode,
            data,
        })
    }
}

fn checked_stride(
    width: u32,
    pixel_format: BroadcastPixelFormat,
) -> Result<usize, MediaPayloadError> {
    (width as usize)
        .checked_mul(pixel_format.bytes_per_pixel())
        .ok_or_else(|| MediaPayloadError::new("video stride overflow"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastAudioSampleFormat {
    F32Interleaved,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BroadcastAudioPayload {
    pub sample_rate_hz: u32,
    pub channels: u8,
    pub sample_format: BroadcastAudioSampleFormat,
    pub sample_span: AudioSampleSpan,
    pub samples: Vec<f32>,
}

impl BroadcastAudioPayload {
    pub fn new_f32_interleaved(
        sample_rate_hz: u32,
        channels: u8,
        sample_span: AudioSampleSpan,
        samples: Vec<f32>,
    ) -> Result<Self, MediaPayloadError> {
        if sample_rate_hz != BROADCAST_AUDIO_SAMPLE_RATE_HZ {
            return Err(MediaPayloadError::new(format!(
                "audio sample rate {sample_rate_hz} is not broadcast 48 kHz"
            )));
        }
        if !(1..=MAX_AUDIO_CHANNELS).contains(&channels) {
            return Err(MediaPayloadError::new(format!(
                "audio channels {channels} outside 1..={MAX_AUDIO_CHANNELS}"
            )));
        }

        let expected = sample_span
            .len()
            .checked_mul(channels as usize)
            .ok_or_else(|| MediaPayloadError::new("audio buffer size overflow"))?;
        if samples.len() != expected {
            return Err(MediaPayloadError::new(format!(
                "audio buffer has {} samples, expected {expected}",
                samples.len()
            )));
        }

        Ok(Self {
            sample_rate_hz,
            channels,
            sample_format: BroadcastAudioSampleFormat::F32Interleaved,
            sample_span,
            samples,
        })
    }

    pub fn silence_for_request(
        request: &AudioDecodeRequest,
        channels: u8,
    ) -> Result<Self, MediaPayloadError> {
        let samples = vec![0.0; request.sample_span.len() * channels as usize];
        Self::new_f32_interleaved(
            request.sample_rate_hz,
            channels,
            request.sample_span,
            samples,
        )
    }

    pub fn sample_frames(&self) -> usize {
        self.sample_span.len()
    }
}

impl DecodedProgramFrame<BroadcastVideoPayload, BroadcastAudioPayload> {
    pub fn validate_payload_contract(&self) -> Result<(), MediaPayloadError> {
        for bus in &self.audio {
            let Some(payload) = bus.payload.as_ref() else {
                continue;
            };
            if payload.sample_rate_hz != bus.sample_rate_hz {
                return Err(MediaPayloadError::new(format!(
                    "audio payload '{}' sample rate {} does not match bus {}",
                    bus.layer_id, payload.sample_rate_hz, bus.sample_rate_hz
                )));
            }
            if payload.sample_span != bus.sample_span {
                return Err(MediaPayloadError::new(format!(
                    "audio payload '{}' sample span {:?} does not match bus {:?}",
                    bus.layer_id, payload.sample_span, bus.sample_span
                )));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::backend::{
        DecodedAudioBus, DecodedProgramFrame, DecodedVideoLayer, FrameDecodePlan,
    };
    use crate::broadcast::timebase::{FrameNumber, FrameRange, Timebase};
    use crate::broadcast::{
        BroadcastFrameScheduler, BroadcastPlaybackSource, BroadcastProgramGraph,
        BroadcastRenderPlan,
    };

    fn source() -> BroadcastPlaybackSource {
        BroadcastPlaybackSource {
            project_id: "project".into(),
            virtual_shot_id: "shot".into(),
            clip_id: "clip".into(),
            source_range: FrameRange::new(FrameNumber(100), FrameNumber(200)),
            source_timebase: Timebase::from_source_fps(25.0),
            has_video: true,
            has_audio: true,
            audio_channels: 1,
        }
    }

    fn decode_plan() -> FrameDecodePlan {
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source());
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        FrameDecodePlan::from_scheduled(&render_plan, scheduler.schedule_frame(FrameNumber(100)))
    }

    #[test]
    fn video_payload_rejects_wrong_rgba_buffer_size() {
        let err = BroadcastVideoPayload::new_rgba8(2, 2, vec![0; 15]).unwrap_err();
        assert!(err.message.contains("expected 16"));
    }

    #[test]
    fn video_payload_accepts_explicit_broadcast_layout() {
        let payload = BroadcastVideoPayload::with_layout(
            2,
            2,
            8,
            BroadcastPixelFormat::Bgra8,
            BroadcastColorSpace::Bt709,
            BroadcastScanMode::Progressive,
            vec![0; 16],
        )
        .unwrap();

        assert_eq!(payload.stride_bytes, 8);
        assert_eq!(payload.pixel_format, BroadcastPixelFormat::Bgra8);
    }

    #[test]
    fn audio_payload_uses_request_sample_span() {
        let plan = decode_plan();
        let request = &plan.audio[0];
        let payload = BroadcastAudioPayload::silence_for_request(request, 1).unwrap();

        assert_eq!(payload.sample_rate_hz, BROADCAST_AUDIO_SAMPLE_RATE_HZ);
        assert_eq!(payload.sample_span, request.sample_span);
        assert_eq!(payload.sample_frames(), 1_920);
        assert_eq!(payload.samples.len(), 1_920);
    }

    #[test]
    fn audio_payload_rejects_non_broadcast_sample_rate() {
        let err = BroadcastAudioPayload::new_f32_interleaved(
            44_100,
            1,
            AudioSampleSpan::new(0, 1_920),
            vec![0.0; 1_920],
        )
        .unwrap_err();

        assert!(err.message.contains("48 kHz"));
    }

    #[test]
    fn decoded_frame_payload_contract_rejects_audio_span_mismatch() {
        let plan = decode_plan();
        let request = &plan.audio[0];
        let payload = BroadcastAudioPayload::new_f32_interleaved(
            BROADCAST_AUDIO_SAMPLE_RATE_HZ,
            1,
            AudioSampleSpan::new(1_920, 3_840),
            vec![0.0; 1_920],
        )
        .unwrap();

        let frame = DecodedProgramFrame {
            source_frame: plan.source_frame,
            pts_sec: plan.pts_sec,
            video: plan
                .video
                .iter()
                .map(|request| DecodedVideoLayer {
                    layer_id: request.layer_id.clone(),
                    role: request.role,
                    source_frame: request.source_frame,
                    pts_sec: request.pts_sec,
                    media_seek_sec: request.media_seek_sec,
                    payload: None,
                })
                .collect(),
            audio: vec![DecodedAudioBus {
                layer_id: request.layer_id.clone(),
                channel: request.channel,
                mix: request.mix,
                source_frame: request.source_frame,
                pts_sec: request.pts_sec,
                media_seek_sec: request.media_seek_sec,
                sample_rate_hz: request.sample_rate_hz,
                sample_span: request.sample_span,
                payload: Some(payload),
            }],
            markers: Vec::new(),
            effects: Vec::new(),
        };

        let err = frame.validate_payload_contract().unwrap_err();
        assert!(err.message.contains("sample span"));
    }
}
