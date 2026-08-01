use serde::{Deserialize, Serialize};

use crate::event::BroadcastEvent;
use crate::model::{AudioRuntime, FrameNumber, FrameRange, Timebase, TransportStatus, VideoFormat};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BroadcastPlayerProtocolEvent {
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
    ExecutionRangeChanged {
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

pub fn validate_broadcast_player_protocol_event(event: &BroadcastEvent) -> Result<(), String> {
    map_broadcast_player_protocol_event(event).map(|_| ())
}

pub fn map_broadcast_player_protocol_event(
    event: &BroadcastEvent,
) -> Result<BroadcastPlayerProtocolEvent, String> {
    match event {
        BroadcastEvent::CommandAccepted {
            command_id,
            command_name,
        } => Ok(BroadcastPlayerProtocolEvent::CommandAccepted {
            command_id: command_id.clone(),
            command_name: command_name.clone(),
        }),
        BroadcastEvent::CommandRejected {
            command_id,
            command_name,
            reason,
        } => Ok(BroadcastPlayerProtocolEvent::CommandRejected {
            command_id: command_id.clone(),
            command_name: command_name.clone(),
            reason: reason.clone(),
        }),
        BroadcastEvent::SourceReady { source_id } => {
            Ok(BroadcastPlayerProtocolEvent::SourceReady {
                source_id: source_id.clone(),
            })
        }
        BroadcastEvent::SourcePreloaded { source_id } => {
            Ok(BroadcastPlayerProtocolEvent::SourcePreloaded {
                source_id: source_id.clone(),
            })
        }
        BroadcastEvent::ActiveSourceChanged { source_id } => {
            Ok(BroadcastPlayerProtocolEvent::ActiveSourceChanged {
                source_id: source_id.clone(),
            })
        }
        BroadcastEvent::SourceSnapshotReloaded {
            source_id,
            source_revision,
        } => Ok(BroadcastPlayerProtocolEvent::SourceSnapshotReloaded {
            source_id: source_id.clone(),
            source_revision: *source_revision,
        }),
        BroadcastEvent::SourceFailed { source_id, reason } => {
            Ok(BroadcastPlayerProtocolEvent::SourceFailed {
                source_id: source_id.clone(),
                reason: reason.clone(),
            })
        }
        BroadcastEvent::CarrierPositionChanged {
            source_id,
            frame,
            range,
            timebase,
            status,
        } => Ok(BroadcastPlayerProtocolEvent::CarrierPositionChanged {
            source_id: source_id.clone(),
            frame: *frame,
            range: *range,
            timebase: *timebase,
            status: *status,
        }),
        BroadcastEvent::TransportStatusChanged { status } => {
            Ok(BroadcastPlayerProtocolEvent::TransportStatusChanged { status: *status })
        }
        BroadcastEvent::RangeChanged { range } => {
            Ok(BroadcastPlayerProtocolEvent::ExecutionRangeChanged { range: *range })
        }
        BroadcastEvent::VideoRuntimeChanged {
            video_format,
            drop_frame_mode,
        } => Ok(BroadcastPlayerProtocolEvent::VideoRuntimeChanged {
            video_format: video_format.clone(),
            drop_frame_mode: *drop_frame_mode,
        }),
        BroadcastEvent::PlaybackBoundaryReached { frame } => {
            Ok(BroadcastPlayerProtocolEvent::PlaybackBoundaryReached { frame: *frame })
        }
        BroadcastEvent::FramePresented { frame } => {
            Ok(BroadcastPlayerProtocolEvent::FramePresented { frame: *frame })
        }
        BroadcastEvent::DroppedFrame { expected_frame } => {
            Ok(BroadcastPlayerProtocolEvent::DroppedFrame {
                expected_frame: *expected_frame,
            })
        }
        BroadcastEvent::AudioLevelChanged {
            track_id,
            peak_dbfs_x100,
        } => Ok(BroadcastPlayerProtocolEvent::AudioLevelChanged {
            track_id: track_id.clone(),
            peak_dbfs_x100: *peak_dbfs_x100,
        }),
        BroadcastEvent::AudioRuntimeChanged { audio_runtime } => {
            Ok(BroadcastPlayerProtocolEvent::AudioRuntimeChanged {
                audio_runtime: audio_runtime.clone(),
            })
        }
        BroadcastEvent::AVSyncWarning { offset_frames } => {
            Ok(BroadcastPlayerProtocolEvent::AVSyncWarning {
                offset_frames: *offset_frames,
            })
        }
        BroadcastEvent::BufferStateChanged { buffered_frames } => {
            Ok(BroadcastPlayerProtocolEvent::BufferStateChanged {
                buffered_frames: *buffered_frames,
            })
        }
        BroadcastEvent::DecodeWarning { message } => {
            Ok(BroadcastPlayerProtocolEvent::DecodeWarning {
                message: message.clone(),
            })
        }
        BroadcastEvent::PlaybackError { message } => {
            Ok(BroadcastPlayerProtocolEvent::PlaybackError {
                message: message.clone(),
            })
        }
    }
}
