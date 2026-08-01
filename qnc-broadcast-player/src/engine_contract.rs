use std::fmt;

use serde::{Deserialize, Serialize};

use crate::event::BroadcastEvent;
use crate::model::{AudioFormat, FrameNumber, SourceRuntime, Timebase, VideoFormat};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BroadcastEngineErrorKind {
    SourceOpen,
    VideoDecode,
    VideoPresent,
    AudioOutput,
    Schedule,
    Contract,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BroadcastEngineError {
    pub kind: BroadcastEngineErrorKind,
    pub source_id: Option<String>,
    pub frame: Option<FrameNumber>,
    pub message: String,
}

impl BroadcastEngineError {
    pub fn new(kind: BroadcastEngineErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            source_id: None,
            frame: None,
            message: message.into(),
        }
    }

    pub fn with_source_id(mut self, source_id: impl Into<String>) -> Self {
        self.source_id = Some(source_id.into());
        self
    }

    pub fn with_frame(mut self, frame: FrameNumber) -> Self {
        self.frame = Some(frame);
        self
    }

    pub fn to_event(&self) -> BroadcastEvent {
        match self.kind {
            BroadcastEngineErrorKind::SourceOpen => BroadcastEvent::SourceFailed {
                source_id: self
                    .source_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                reason: self.message.clone(),
            },
            BroadcastEngineErrorKind::VideoDecode => BroadcastEvent::DecodeWarning {
                message: self.message.clone(),
            },
            BroadcastEngineErrorKind::VideoPresent
            | BroadcastEngineErrorKind::AudioOutput
            | BroadcastEngineErrorKind::Schedule
            | BroadcastEngineErrorKind::Contract => BroadcastEvent::PlaybackError {
                message: self.message.clone(),
            },
        }
    }
}

impl fmt::Display for BroadcastEngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for BroadcastEngineError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineSourceHandle {
    pub source_id: String,
    pub source_revision: Option<u64>,
    pub duration_frames: FrameNumber,
    pub timebase: Timebase,
    pub video_format: Option<VideoFormat>,
    pub audio_format: Option<AudioFormat>,
}

impl EngineSourceHandle {
    pub fn from_source_runtime(source: &SourceRuntime, source_revision: Option<u64>) -> Self {
        Self {
            source_id: source.source_id.clone(),
            source_revision,
            duration_frames: source.duration_frames,
            timebase: source.timebase,
            video_format: source.video_format.clone(),
            audio_format: source.audio_format.clone(),
        }
    }

    pub fn require_frame(&self, frame: FrameNumber) -> Result<(), BroadcastEngineError> {
        if frame > self.duration_frames {
            return Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::Contract,
                format!(
                    "frame {frame} is outside source duration {}",
                    self.duration_frames
                ),
            )
            .with_source_id(self.source_id.clone())
            .with_frame(frame));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineFrameRequest {
    pub source_id: String,
    pub frame: FrameNumber,
    pub timebase: Timebase,
}

impl EngineFrameRequest {
    pub fn new(
        source: &EngineSourceHandle,
        frame: FrameNumber,
    ) -> Result<Self, BroadcastEngineError> {
        source.require_frame(frame)?;
        Ok(Self {
            source_id: source.source_id.clone(),
            frame,
            timebase: source.timebase,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedVideoFrame<T> {
    pub source_id: String,
    pub frame: FrameNumber,
    pub video_format: Option<VideoFormat>,
    pub payload: T,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFramePacket<T> {
    pub source_id: String,
    pub start_frame: FrameNumber,
    pub frame_count: FrameNumber,
    pub audio_format: Option<AudioFormat>,
    pub payload: T,
}

pub trait SourceOpenAdapter {
    fn open_source(
        &mut self,
        source: &SourceRuntime,
        source_revision: Option<u64>,
    ) -> Result<EngineSourceHandle, BroadcastEngineError>;

    fn close_source(&mut self, source_id: &str) -> Result<(), BroadcastEngineError>;
}

pub trait VideoDecodeAdapter {
    type VideoFrame;

    fn prepare_video(
        &mut self,
        source: &EngineSourceHandle,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError>;

    fn decode_video_frame(
        &mut self,
        request: EngineFrameRequest,
    ) -> Result<DecodedVideoFrame<Self::VideoFrame>, BroadcastEngineError>;
}

pub trait AudioOutputAdapter {
    type AudioPacket;

    fn prepare_audio(
        &mut self,
        source: &EngineSourceHandle,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError>;

    fn render_audio_for_frame(
        &mut self,
        request: EngineFrameRequest,
    ) -> Result<AudioFramePacket<Self::AudioPacket>, BroadcastEngineError>;

    fn submit_audio_packet(
        &mut self,
        packet: AudioFramePacket<Self::AudioPacket>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError>;

    fn stop_audio(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError>;
}

pub trait FramePresenter {
    type VideoFrame;

    fn present_frame(
        &mut self,
        frame: DecodedVideoFrame<Self::VideoFrame>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError>;
}

pub trait MonotonicScheduler {
    fn start_at_frame(
        &mut self,
        frame: FrameNumber,
        timebase: Timebase,
        rate_num: i32,
        rate_den: u32,
    ) -> Result<(), BroadcastEngineError>;

    fn stop(&mut self) -> Result<(), BroadcastEngineError>;

    fn next_due_frame(&mut self) -> Result<Option<FrameNumber>, BroadcastEngineError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AudioFormat, ColorSpace, FieldMode, Timebase, VideoFormat};

    #[test]
    fn engine_contract_models_are_frame_based_and_neutral() {
        let source = source_runtime();
        let handle = EngineSourceHandle::from_source_runtime(&source, Some(7));
        let request = EngineFrameRequest::new(&handle, 12).unwrap();

        let handle_value = serde_json::to_value(&handle).unwrap();
        let request_value = serde_json::to_value(&request).unwrap();
        let handle_fields = handle_value.as_object().expect("source handle object");
        let request_fields = request_value.as_object().expect("frame request object");

        assert_eq!(handle_fields.len(), 6);
        for field in [
            "source_id",
            "source_revision",
            "duration_frames",
            "timebase",
            "video_format",
            "audio_format",
        ] {
            assert!(handle_fields.contains_key(field), "missing field: {field}");
        }
        assert_eq!(request_fields.len(), 3);
        for field in ["source_id", "frame", "timebase"] {
            assert!(request_fields.contains_key(field), "missing field: {field}");
        }
    }

    #[test]
    fn source_open_adapter_returns_handle_without_media_location() {
        let mut adapter = FakeSourceOpenAdapter;
        let source = source_runtime();

        let handle = adapter.open_source(&source, Some(3)).unwrap();

        assert_eq!(handle.source_id, "src");
        assert_eq!(handle.duration_frames, 100);
        assert_eq!(handle.source_revision, Some(3));
        adapter.close_source("src").unwrap();
    }

    #[test]
    fn video_decode_and_present_are_connected_by_frame_request() {
        let source = EngineSourceHandle::from_source_runtime(&source_runtime(), None);
        let request = EngineFrameRequest::new(&source, 14).unwrap();
        let mut decoder = FakeVideoDecodeAdapter;
        let mut presenter = FakeFramePresenter;

        decoder.prepare_video(&source).unwrap();
        let frame = decoder.decode_video_frame(request).unwrap();
        let events = presenter.present_frame(frame).unwrap();

        assert!(
            events
                .iter()
                .any(|event| matches!(event, BroadcastEvent::FramePresented { frame: 14 }))
        );
    }

    #[test]
    fn audio_output_contract_is_aligned_to_carrier_frame() {
        let source = EngineSourceHandle::from_source_runtime(&source_runtime(), None);
        let request = EngineFrameRequest::new(&source, 15).unwrap();
        let mut audio = FakeAudioOutputAdapter;

        audio.prepare_audio(&source).unwrap();
        let packet = audio.render_audio_for_frame(request).unwrap();
        assert_eq!(packet.start_frame, 15);
        let events = audio.submit_audio_packet(packet).unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            BroadcastEvent::AudioLevelChanged {
                track_id,
                peak_dbfs_x100: -1200
            } if track_id == "monitor"
        )));
    }

    #[test]
    fn scheduler_contract_returns_due_frames_only() {
        let mut scheduler = FakeScheduler::default();

        scheduler
            .start_at_frame(20, Timebase::new(25, 1).unwrap(), 1, 1)
            .unwrap();

        assert_eq!(scheduler.next_due_frame().unwrap(), Some(20));
        assert_eq!(scheduler.next_due_frame().unwrap(), Some(21));
        scheduler.stop().unwrap();
        assert_eq!(scheduler.next_due_frame().unwrap(), None);
    }

    #[test]
    fn engine_error_maps_to_neutral_event() {
        let error = BroadcastEngineError::new(BroadcastEngineErrorKind::SourceOpen, "open failed")
            .with_source_id("src");

        assert!(matches!(
            error.to_event(),
            BroadcastEvent::SourceFailed { source_id, reason }
                if source_id == "src" && reason == "open failed"
        ));
    }

    fn source_runtime() -> SourceRuntime {
        SourceRuntime::new("src", 100, Timebase::new(25, 1).unwrap())
            .unwrap()
            .with_video_format(
                VideoFormat::new(1920, 1080, FieldMode::Progressive, ColorSpace::Rec709).unwrap(),
            )
            .with_audio_format(AudioFormat::new(48_000, 2).unwrap())
    }

    struct FakeSourceOpenAdapter;

    impl SourceOpenAdapter for FakeSourceOpenAdapter {
        fn open_source(
            &mut self,
            source: &SourceRuntime,
            source_revision: Option<u64>,
        ) -> Result<EngineSourceHandle, BroadcastEngineError> {
            Ok(EngineSourceHandle::from_source_runtime(
                source,
                source_revision,
            ))
        }

        fn close_source(&mut self, _source_id: &str) -> Result<(), BroadcastEngineError> {
            Ok(())
        }
    }

    struct FakeVideoDecodeAdapter;

    impl VideoDecodeAdapter for FakeVideoDecodeAdapter {
        type VideoFrame = ();

        fn prepare_video(
            &mut self,
            _source: &EngineSourceHandle,
        ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
            Ok(Vec::new())
        }

        fn decode_video_frame(
            &mut self,
            request: EngineFrameRequest,
        ) -> Result<DecodedVideoFrame<Self::VideoFrame>, BroadcastEngineError> {
            Ok(DecodedVideoFrame {
                source_id: request.source_id,
                frame: request.frame,
                video_format: None,
                payload: (),
            })
        }
    }

    struct FakeFramePresenter;

    impl FramePresenter for FakeFramePresenter {
        type VideoFrame = ();

        fn present_frame(
            &mut self,
            frame: DecodedVideoFrame<Self::VideoFrame>,
        ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
            Ok(vec![BroadcastEvent::FramePresented { frame: frame.frame }])
        }
    }

    struct FakeAudioOutputAdapter;

    impl AudioOutputAdapter for FakeAudioOutputAdapter {
        type AudioPacket = ();

        fn prepare_audio(
            &mut self,
            _source: &EngineSourceHandle,
        ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
            Ok(Vec::new())
        }

        fn render_audio_for_frame(
            &mut self,
            request: EngineFrameRequest,
        ) -> Result<AudioFramePacket<Self::AudioPacket>, BroadcastEngineError> {
            Ok(AudioFramePacket {
                source_id: request.source_id,
                start_frame: request.frame,
                frame_count: 1,
                audio_format: None,
                payload: (),
            })
        }

        fn submit_audio_packet(
            &mut self,
            _packet: AudioFramePacket<Self::AudioPacket>,
        ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
            Ok(vec![BroadcastEvent::AudioLevelChanged {
                track_id: "monitor".to_string(),
                peak_dbfs_x100: -1200,
            }])
        }

        fn stop_audio(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
            Ok(Vec::new())
        }
    }

    #[derive(Default)]
    struct FakeScheduler {
        next_frame: Option<FrameNumber>,
    }

    impl MonotonicScheduler for FakeScheduler {
        fn start_at_frame(
            &mut self,
            frame: FrameNumber,
            _timebase: Timebase,
            _rate_num: i32,
            rate_den: u32,
        ) -> Result<(), BroadcastEngineError> {
            if rate_den == 0 {
                return Err(BroadcastEngineError::new(
                    BroadcastEngineErrorKind::Schedule,
                    "rate_den must be greater than zero",
                ));
            }
            self.next_frame = Some(frame);
            Ok(())
        }

        fn stop(&mut self) -> Result<(), BroadcastEngineError> {
            self.next_frame = None;
            Ok(())
        }

        fn next_due_frame(&mut self) -> Result<Option<FrameNumber>, BroadcastEngineError> {
            let Some(frame) = self.next_frame else {
                return Ok(None);
            };
            self.next_frame = Some(frame.saturating_add(1));
            Ok(Some(frame))
        }
    }
}
