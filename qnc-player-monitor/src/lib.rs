use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use qnc_broadcast_player::{
    AudioRuntime, BroadcastPlayerProtocolEvent, FrameNumber, Timebase, TransportStatus, VideoFormat,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorPixelLayout {
    Rgb24,
}

impl MonitorPixelLayout {
    pub const fn bytes_per_pixel(self) -> u64 {
        match self {
            Self::Rgb24 => 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorFrameBuffer {
    pub source_id: Option<String>,
    pub frame: FrameNumber,
    pub video_format: VideoFormat,
    pub pixel_layout: MonitorPixelLayout,
    pub bytes: Vec<u8>,
}

impl MonitorFrameBuffer {
    pub fn rgb24(
        source_id: Option<String>,
        frame: FrameNumber,
        video_format: VideoFormat,
        bytes: Vec<u8>,
    ) -> Result<Self, MonitorError> {
        Self::new(
            source_id,
            frame,
            video_format,
            MonitorPixelLayout::Rgb24,
            bytes,
        )
    }

    pub fn new(
        source_id: Option<String>,
        frame: FrameNumber,
        video_format: VideoFormat,
        pixel_layout: MonitorPixelLayout,
        bytes: Vec<u8>,
    ) -> Result<Self, MonitorError> {
        let expected_bytes = expected_buffer_bytes(&video_format, pixel_layout)?;
        if bytes.len() != expected_bytes {
            return Err(MonitorError::InvalidBufferSize {
                expected_bytes,
                actual_bytes: bytes.len(),
            });
        }
        Ok(Self {
            source_id,
            frame,
            video_format,
            pixel_layout,
            bytes,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerMonitorState {
    pub ready_source_id: Option<String>,
    pub preloaded_source_id: Option<String>,
    pub active_source_id: Option<String>,
    pub failed_source_id: Option<String>,
    pub revised_source_id: Option<String>,
    pub source_revision: Option<u64>,
    pub carrier_frame: Option<FrameNumber>,
    pub presented_frame: Option<FrameNumber>,
    pub boundary_frame: Option<FrameNumber>,
    pub transport_status: TransportStatus,
    pub timebase: Option<Timebase>,
    pub video_format: Option<VideoFormat>,
    pub drop_frame_mode: bool,
    pub buffered_frames: FrameNumber,
    pub last_frame_buffer: Option<MonitorFrameBuffer>,
    pub expected_dropped_frame: Option<FrameNumber>,
    pub audio_levels: BTreeMap<String, i32>,
    pub audio_runtime: AudioRuntime,
    pub av_sync_offset_frames: Option<i64>,
    pub last_warning: Option<String>,
    pub last_error: Option<String>,
    pub event_revision: u64,
    pub frame_revision: u64,
}

impl Default for PlayerMonitorState {
    fn default() -> Self {
        Self {
            ready_source_id: None,
            preloaded_source_id: None,
            active_source_id: None,
            failed_source_id: None,
            revised_source_id: None,
            source_revision: None,
            carrier_frame: None,
            presented_frame: None,
            boundary_frame: None,
            transport_status: TransportStatus::Empty,
            timebase: None,
            video_format: None,
            drop_frame_mode: false,
            buffered_frames: 0,
            last_frame_buffer: None,
            expected_dropped_frame: None,
            audio_levels: BTreeMap::new(),
            audio_runtime: AudioRuntime::default(),
            av_sync_offset_frames: None,
            last_warning: None,
            last_error: None,
            event_revision: 0,
            frame_revision: 0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlayerMonitor {
    state: PlayerMonitorState,
}

impl PlayerMonitor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> &PlayerMonitorState {
        &self.state
    }

    pub fn reset(&mut self) {
        self.state = PlayerMonitorState::default();
    }

    pub fn accept_frame_buffer(
        &mut self,
        frame_buffer: MonitorFrameBuffer,
    ) -> Result<(), MonitorError> {
        let expected_bytes =
            expected_buffer_bytes(&frame_buffer.video_format, frame_buffer.pixel_layout)?;
        if frame_buffer.bytes.len() != expected_bytes {
            return Err(MonitorError::InvalidBufferSize {
                expected_bytes,
                actual_bytes: frame_buffer.bytes.len(),
            });
        }
        self.state.active_source_id = frame_buffer.source_id.clone();
        self.state.presented_frame = Some(frame_buffer.frame);
        self.state.video_format = Some(frame_buffer.video_format.clone());
        self.state.last_frame_buffer = Some(frame_buffer);
        self.state.frame_revision = self.state.frame_revision.saturating_add(1);
        Ok(())
    }

    pub fn apply_event(&mut self, event: &BroadcastPlayerProtocolEvent) {
        self.state.event_revision = self.state.event_revision.saturating_add(1);
        match event {
            BroadcastPlayerProtocolEvent::CommandAccepted { .. } => {}
            BroadcastPlayerProtocolEvent::CommandRejected { reason, .. } => {
                self.state.last_warning = Some(reason.clone());
            }
            BroadcastPlayerProtocolEvent::SourceReady { source_id } => {
                self.state.ready_source_id = Some(source_id.clone());
                self.state.last_error = None;
            }
            BroadcastPlayerProtocolEvent::SourcePreloaded { source_id } => {
                self.state.preloaded_source_id = Some(source_id.clone());
            }
            BroadcastPlayerProtocolEvent::ActiveSourceChanged { source_id } => {
                self.state.active_source_id = source_id.clone();
                self.state.source_revision = None;
                self.state.carrier_frame = None;
                self.state.presented_frame = None;
                self.state.boundary_frame = None;
                self.state.last_frame_buffer = None;
            }
            BroadcastPlayerProtocolEvent::SourceSnapshotReloaded {
                source_id,
                source_revision,
            } => {
                self.state.revised_source_id = Some(source_id.clone());
                self.state.source_revision = Some(*source_revision);
            }
            BroadcastPlayerProtocolEvent::SourceFailed { source_id, reason } => {
                self.state.failed_source_id = Some(source_id.clone());
                self.state.last_error = Some(reason.clone());
            }
            BroadcastPlayerProtocolEvent::CarrierPositionChanged {
                source_id,
                frame,
                timebase,
                status,
                ..
            } => {
                self.state.active_source_id = source_id.clone();
                self.state.carrier_frame = Some(*frame);
                self.state.timebase = *timebase;
                self.state.transport_status = *status;
            }
            BroadcastPlayerProtocolEvent::TransportStatusChanged { status } => {
                self.state.transport_status = *status;
            }
            BroadcastPlayerProtocolEvent::ExecutionRangeChanged { .. } => {}
            BroadcastPlayerProtocolEvent::VideoRuntimeChanged {
                video_format,
                drop_frame_mode,
            } => {
                self.state.video_format = video_format.clone();
                self.state.drop_frame_mode = *drop_frame_mode;
            }
            BroadcastPlayerProtocolEvent::PlaybackBoundaryReached { frame } => {
                self.state.boundary_frame = Some(*frame);
            }
            BroadcastPlayerProtocolEvent::FramePresented { frame } => {
                self.state.presented_frame = Some(*frame);
                self.state.frame_revision = self.state.frame_revision.saturating_add(1);
            }
            BroadcastPlayerProtocolEvent::DroppedFrame { expected_frame } => {
                self.state.expected_dropped_frame = Some(*expected_frame);
            }
            BroadcastPlayerProtocolEvent::AudioLevelChanged {
                track_id,
                peak_dbfs_x100,
            } => {
                self.state
                    .audio_levels
                    .insert(track_id.clone(), *peak_dbfs_x100);
            }
            BroadcastPlayerProtocolEvent::AudioRuntimeChanged { audio_runtime } => {
                self.state.audio_runtime = audio_runtime.clone();
            }
            BroadcastPlayerProtocolEvent::AVSyncWarning { offset_frames } => {
                self.state.av_sync_offset_frames = Some(*offset_frames);
            }
            BroadcastPlayerProtocolEvent::BufferStateChanged { buffered_frames } => {
                self.state.buffered_frames = *buffered_frames;
            }
            BroadcastPlayerProtocolEvent::DecodeWarning { message } => {
                self.state.last_warning = Some(message.clone());
            }
            BroadcastPlayerProtocolEvent::PlaybackError { message } => {
                self.state.last_error = Some(message.clone());
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonitorError {
    InvalidBufferSize {
        expected_bytes: usize,
        actual_bytes: usize,
    },
    BufferSizeOverflow,
}

impl fmt::Display for MonitorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBufferSize {
                expected_bytes,
                actual_bytes,
            } => write!(
                formatter,
                "monitor frame buffer expected {expected_bytes} bytes, got {actual_bytes}"
            ),
            Self::BufferSizeOverflow => write!(formatter, "monitor frame buffer size overflow"),
        }
    }
}

impl Error for MonitorError {}

fn expected_buffer_bytes(
    video_format: &VideoFormat,
    pixel_layout: MonitorPixelLayout,
) -> Result<usize, MonitorError> {
    let pixels = u64::from(video_format.width)
        .checked_mul(u64::from(video_format.height))
        .ok_or(MonitorError::BufferSizeOverflow)?;
    let bytes = pixels
        .checked_mul(pixel_layout.bytes_per_pixel())
        .ok_or(MonitorError::BufferSizeOverflow)?;
    usize::try_from(bytes).map_err(|_| MonitorError::BufferSizeOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qnc_broadcast_player::{ColorSpace, FieldMode, FrameRange, PixelAspect};

    #[test]
    fn monitor_starts_empty() {
        let monitor = PlayerMonitor::new();

        assert_eq!(monitor.state().transport_status, TransportStatus::Empty);
        assert_eq!(monitor.state().carrier_frame, None);
        assert_eq!(monitor.state().presented_frame, None);
        assert_eq!(monitor.state().last_frame_buffer, None);
    }

    #[test]
    fn monitor_projects_carrier_event_without_owning_range() {
        let mut monitor = PlayerMonitor::new();

        monitor.apply_event(&BroadcastPlayerProtocolEvent::CarrierPositionChanged {
            source_id: Some("src-a".to_string()),
            frame: 12,
            range: Some(FrameRange::new(10, 20).unwrap()),
            timebase: Some(Timebase::new(25, 1).unwrap()),
            status: TransportStatus::Playing,
        });

        assert_eq!(monitor.state().active_source_id.as_deref(), Some("src-a"));
        assert_eq!(monitor.state().carrier_frame, Some(12));
        assert_eq!(monitor.state().transport_status, TransportStatus::Playing);
        assert_eq!(
            monitor.state().timebase,
            Some(Timebase::new(25, 1).unwrap())
        );
        assert_eq!(monitor.state().event_revision, 1);
    }

    #[test]
    fn monitor_does_not_treat_preload_as_active_source() {
        let mut monitor = PlayerMonitor::new();

        monitor.apply_event(&BroadcastPlayerProtocolEvent::SourcePreloaded {
            source_id: "src-b".to_string(),
        });

        assert_eq!(
            monitor.state().preloaded_source_id.as_deref(),
            Some("src-b")
        );
        assert_eq!(monitor.state().active_source_id, None);
    }

    #[test]
    fn monitor_projects_presented_frame_event() {
        let mut monitor = PlayerMonitor::new();

        monitor.apply_event(&BroadcastPlayerProtocolEvent::FramePresented { frame: 44 });

        assert_eq!(monitor.state().presented_frame, Some(44));
        assert_eq!(monitor.state().frame_revision, 1);
    }

    #[test]
    fn monitor_accepts_valid_rgb_frame_buffer() {
        let mut monitor = PlayerMonitor::new();
        let frame_buffer = MonitorFrameBuffer::rgb24(
            Some("src-a".to_string()),
            7,
            video_format(2, 2),
            vec![0; 12],
        )
        .unwrap();

        monitor.accept_frame_buffer(frame_buffer).unwrap();

        assert_eq!(monitor.state().active_source_id.as_deref(), Some("src-a"));
        assert_eq!(monitor.state().presented_frame, Some(7));
        assert_eq!(monitor.state().frame_revision, 1);
        assert_eq!(
            monitor
                .state()
                .last_frame_buffer
                .as_ref()
                .unwrap()
                .bytes
                .len(),
            12
        );
    }

    #[test]
    fn monitor_rejects_invalid_rgb_frame_buffer_size() {
        let error =
            MonitorFrameBuffer::rgb24(None, 7, video_format(2, 2), vec![0; 11]).unwrap_err();

        assert_eq!(
            error,
            MonitorError::InvalidBufferSize {
                expected_bytes: 12,
                actual_bytes: 11
            }
        );
    }

    #[test]
    fn monitor_projects_output_telemetry() {
        let mut monitor = PlayerMonitor::new();

        monitor.apply_event(&BroadcastPlayerProtocolEvent::DroppedFrame { expected_frame: 8 });
        monitor
            .apply_event(&BroadcastPlayerProtocolEvent::BufferStateChanged { buffered_frames: 3 });
        monitor.apply_event(&BroadcastPlayerProtocolEvent::AVSyncWarning { offset_frames: -1 });

        assert_eq!(monitor.state().expected_dropped_frame, Some(8));
        assert_eq!(monitor.state().buffered_frames, 3);
        assert_eq!(monitor.state().av_sync_offset_frames, Some(-1));
        assert_eq!(monitor.state().event_revision, 3);
    }

    #[test]
    fn monitor_projects_audio_and_video_runtime_metadata() {
        let mut monitor = PlayerMonitor::new();

        monitor.apply_event(&BroadcastPlayerProtocolEvent::VideoRuntimeChanged {
            video_format: Some(video_format(16, 9)),
            drop_frame_mode: true,
        });
        monitor.apply_event(&BroadcastPlayerProtocolEvent::AudioLevelChanged {
            track_id: "mix".to_string(),
            peak_dbfs_x100: -600,
        });
        monitor.apply_event(&BroadcastPlayerProtocolEvent::AudioRuntimeChanged {
            audio_runtime: AudioRuntime {
                monitor_volume_millibels: -300,
                ..AudioRuntime::default()
            },
        });

        assert_eq!(monitor.state().video_format, Some(video_format(16, 9)));
        assert!(monitor.state().drop_frame_mode);
        assert_eq!(monitor.state().audio_levels.get("mix"), Some(&-600));
        assert_eq!(monitor.state().audio_runtime.monitor_volume_millibels, -300);
    }

    #[test]
    fn monitor_reset_clears_projection_state() {
        let mut monitor = PlayerMonitor::new();
        monitor.apply_event(&BroadcastPlayerProtocolEvent::FramePresented { frame: 9 });

        monitor.reset();

        assert_eq!(monitor.state(), &PlayerMonitorState::default());
    }

    fn video_format(width: u32, height: u32) -> VideoFormat {
        VideoFormat {
            width,
            height,
            field_mode: FieldMode::Progressive,
            color_space: ColorSpace::Rec709,
            pixel_aspect: PixelAspect::square(),
        }
    }
}
