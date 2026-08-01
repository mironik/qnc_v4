mod command;
mod event;
mod request;

pub use command::{BroadcastPlayerProtocolCommand, validate_broadcast_player_protocol_command};
pub use event::{
    BroadcastPlayerProtocolEvent, map_broadcast_player_protocol_event,
    validate_broadcast_player_protocol_event,
};
pub use request::BroadcastPlaybackRequest;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::BroadcastEvent;
    use crate::model::{FrameRange, SourceRuntime, Timebase, TransportStatus};

    #[test]
    fn protocol_accepts_only_player_executor_commands() {
        for command in [
            BroadcastPlayerProtocolCommand::SetPlaybackRequest {
                request: Box::new(
                    BroadcastPlaybackRequest::new(
                        "request-1",
                        SourceRuntime::new("src", 100, Timebase::new(25, 1).unwrap()).unwrap(),
                    )
                    .unwrap(),
                ),
            },
            BroadcastPlayerProtocolCommand::Play,
            BroadcastPlayerProtocolCommand::Pause,
            BroadcastPlayerProtocolCommand::Stop,
        ] {
            assert!(validate_broadcast_player_protocol_command(&command).is_ok());
        }
    }

    #[test]
    fn protocol_event_maps_runtime_boundary_without_edit_label() {
        let event = map_broadcast_player_protocol_event(&BroadcastEvent::PlaybackBoundaryReached {
            frame: 20,
        })
        .unwrap();

        assert_eq!(
            event,
            BroadcastPlayerProtocolEvent::PlaybackBoundaryReached { frame: 20 }
        );
        let text = serde_json::to_string(&event).unwrap();
        assert!(text.contains("PlaybackBoundaryReached"));
        assert!(!text.contains(concat!("Set", "Out")));
    }

    #[test]
    fn playback_request_contract_is_frame_based_and_source_snapshot_only() {
        let source = SourceRuntime::new("src", 100, Timebase::new(25, 1).unwrap()).unwrap();
        let request = BroadcastPlaybackRequest::new("request-1", source)
            .unwrap()
            .with_range(FrameRange::new(10, 20).unwrap())
            .unwrap()
            .with_rate(1, 1)
            .unwrap();

        request.validate().unwrap();
        let text = serde_json::to_string(&request)
            .unwrap()
            .to_ascii_lowercase();
        assert!(text.contains("start_frame"));
        assert!(text.contains("end_frame"));
        assert!(text.contains("request_id"));
        assert!(!text.contains("path"));
    }

    #[test]
    fn playback_request_rejects_range_outside_source_snapshot() {
        let source = SourceRuntime::new("src", 100, Timebase::new(25, 1).unwrap()).unwrap();

        let err = BroadcastPlaybackRequest::new("request-1", source)
            .unwrap()
            .with_range(FrameRange::new(10, 101).unwrap())
            .unwrap_err();

        assert!(err.contains("outside source duration"));
    }

    #[test]
    fn playback_request_rejects_non_playback_rate() {
        let source = SourceRuntime::new("src", 100, Timebase::new(25, 1).unwrap()).unwrap();

        let err = BroadcastPlaybackRequest::new("request-1", source)
            .unwrap()
            .with_rate(0, 1)
            .unwrap_err();

        assert!(err.contains("rate_num"));
    }

    #[test]
    fn protocol_validates_single_playback_request_payload() {
        let source = SourceRuntime::new("src", 100, Timebase::new(25, 1).unwrap()).unwrap();
        let request = BroadcastPlaybackRequest::new("request-1", source).unwrap();

        assert!(request.validate().is_ok());
    }

    #[test]
    fn protocol_maps_position_event_to_frame_based_event() {
        let event = map_broadcast_player_protocol_event(&BroadcastEvent::CarrierPositionChanged {
            source_id: Some("src".to_string()),
            frame: 12,
            range: Some(FrameRange::new(10, 20).unwrap()),
            timebase: Some(Timebase::new(25, 1).unwrap()),
            status: TransportStatus::Playing,
        })
        .unwrap();

        assert!(matches!(
            event,
            BroadcastPlayerProtocolEvent::CarrierPositionChanged { frame: 12, .. }
        ));
    }
}
