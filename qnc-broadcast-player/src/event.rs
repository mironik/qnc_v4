use serde::{Deserialize, Serialize};

use crate::model::{AudioRuntime, FrameNumber, FrameRange, Timebase, TransportStatus, VideoFormat};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BroadcastEvent {
    CommandAccepted {
        command_id: String,
        command_name: String,
    },
    CommandRejected {
        command_id: String,
        command_name: String,
        reason: String,
    },
    SourceReady {
        source_id: String,
    },
    SourcePreloaded {
        source_id: String,
    },
    ActiveSourceChanged {
        source_id: Option<String>,
    },
    SourceSnapshotReloaded {
        source_id: String,
        source_revision: u64,
    },
    SourceFailed {
        source_id: String,
        reason: String,
    },
    CarrierPositionChanged {
        source_id: Option<String>,
        frame: FrameNumber,
        range: Option<FrameRange>,
        timebase: Option<Timebase>,
        status: TransportStatus,
    },
    TransportStatusChanged {
        status: TransportStatus,
    },
    RangeChanged {
        range: Option<FrameRange>,
    },
    VideoRuntimeChanged {
        video_format: Option<VideoFormat>,
        drop_frame_mode: bool,
    },
    PlaybackBoundaryReached {
        frame: FrameNumber,
    },
    FramePresented {
        frame: FrameNumber,
    },
    DroppedFrame {
        expected_frame: FrameNumber,
    },
    AudioLevelChanged {
        track_id: String,
        peak_dbfs_x100: i32,
    },
    AudioRuntimeChanged {
        audio_runtime: AudioRuntime,
    },
    AVSyncWarning {
        offset_frames: i64,
    },
    BufferStateChanged {
        buffered_frames: FrameNumber,
    },
    DecodeWarning {
        message: String,
    },
    PlaybackError {
        message: String,
    },
}
