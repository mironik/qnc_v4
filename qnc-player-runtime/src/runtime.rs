use std::collections::{BTreeMap, VecDeque};

#[cfg(test)]
use qnc_broadcast_player::model::FrameNumber;
use qnc_broadcast_player::{
    BroadcastEngineError, BroadcastEngineErrorKind, BroadcastEvent, BroadcastPlayerProtocolCommand,
    BroadcastPlayerProtocolEvent, ClockTick, SourceRuntime, map_broadcast_player_protocol_event,
};

use crate::{PlayerRuntimeCommand, PlayerTransport};

pub const DEFAULT_EVENT_QUEUE_LIMIT: usize = 1024;

pub struct BroadcastPlayerRuntime<T>
where
    T: PlayerTransport,
{
    transport: T,
    sources: BTreeMap<String, SourceRuntime>,
    events: VecDeque<BroadcastPlayerProtocolEvent>,
    event_queue_limit: usize,
}

impl<T> BroadcastPlayerRuntime<T>
where
    T: PlayerTransport,
{
    pub fn new(transport: T) -> Self {
        Self::with_event_queue_limit(transport, DEFAULT_EVENT_QUEUE_LIMIT)
    }

    pub fn with_event_queue_limit(transport: T, event_queue_limit: usize) -> Self {
        Self {
            transport,
            sources: BTreeMap::new(),
            events: VecDeque::new(),
            event_queue_limit: event_queue_limit.max(1),
        }
    }

    pub fn dispatch_at(&mut self, command: PlayerRuntimeCommand, now_tick: ClockTick) {
        let command_name = command.command_name().to_string();
        if let Err(reason) = command.validate() {
            self.reject(command.command_id, command_name, reason);
            return;
        }

        match self.execute(&command.command, now_tick) {
            Ok(events) => {
                self.push_protocol_event(BroadcastPlayerProtocolEvent::CommandAccepted {
                    command_id: command.command_id,
                    command_name,
                });
                self.push_runtime_events(events);
            }
            Err(error) => {
                self.reject(command.command_id, command_name, error.to_string());
                self.push_engine_error(error);
            }
        }
    }

    pub fn tick(&mut self, now_tick: ClockTick) {
        match self.transport.tick(now_tick) {
            Ok(events) => self.push_runtime_events(events),
            Err(error) => self.push_engine_error(error),
        }
    }

    pub fn drain_events(&mut self) -> Vec<BroadcastPlayerProtocolEvent> {
        self.events.drain(..).collect()
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn event_queue_limit(&self) -> usize {
        self.event_queue_limit
    }

    fn execute(
        &mut self,
        command: &BroadcastPlayerProtocolCommand,
        now_tick: ClockTick,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        match command {
            BroadcastPlayerProtocolCommand::LoadSource { source } => {
                self.remember_source(source.clone());
                self.transport.load_source(source, None)
            }
            BroadcastPlayerProtocolCommand::PreloadSource { source } => {
                self.remember_source(source.clone());
                self.transport.preload_source(source, None)
            }
            BroadcastPlayerProtocolCommand::SetActiveSource { source_id } => {
                let source = self.source(source_id)?.clone();
                self.transport.set_active_source(&source, None)
            }
            BroadcastPlayerProtocolCommand::UnloadSource => self.transport.unload_source(),
            BroadcastPlayerProtocolCommand::SetPlaybackRequest { request } => {
                request.validate().map_err(contract_error)?;
                self.remember_source(request.source_runtime.clone());
                let mut events = self
                    .transport
                    .set_active_source(&request.source_runtime, None)?;
                events.extend(self.transport.sync_range_runtime(
                    Some(request.execution_range),
                    request.execution_range.start_frame,
                    true,
                )?);
                events.extend(
                    self.transport
                        .apply_request_rate(request.rate_num, request.rate_den)?,
                );
                events.push(BroadcastEvent::AudioRuntimeChanged {
                    audio_runtime: request.audio_runtime.clone(),
                });
                Ok(events)
            }
            BroadcastPlayerProtocolCommand::Play => self.transport.play(now_tick),
            BroadcastPlayerProtocolCommand::Pause => self.transport.pause(),
            BroadcastPlayerProtocolCommand::Stop => self.transport.stop(),
        }
    }

    fn remember_source(&mut self, source: SourceRuntime) {
        self.sources.insert(source.source_id.clone(), source);
    }

    fn source(&self, source_id: &str) -> Result<&SourceRuntime, BroadcastEngineError> {
        self.sources
            .get(source_id)
            .ok_or_else(|| contract_error(format!("source snapshot not registered: {source_id}")))
    }

    fn reject(&mut self, command_id: String, command_name: String, reason: String) {
        self.push_protocol_event(BroadcastPlayerProtocolEvent::CommandRejected {
            command_id,
            command_name,
            reason,
        });
    }

    fn push_runtime_events(&mut self, events: Vec<BroadcastEvent>) {
        for event in events {
            match map_broadcast_player_protocol_event(&event) {
                Ok(protocol_event) => self.push_protocol_event(protocol_event),
                Err(reason) => {
                    self.push_protocol_event(BroadcastPlayerProtocolEvent::PlaybackError {
                        message: reason,
                    });
                }
            }
        }
    }

    fn push_engine_error(&mut self, error: BroadcastEngineError) {
        let event = error.to_event();
        match map_broadcast_player_protocol_event(&event) {
            Ok(protocol_event) => self.push_protocol_event(protocol_event),
            Err(reason) => {
                self.push_protocol_event(BroadcastPlayerProtocolEvent::PlaybackError {
                    message: reason,
                });
            }
        }
    }

    fn push_protocol_event(&mut self, event: BroadcastPlayerProtocolEvent) {
        if self.events.len() >= self.event_queue_limit {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }
}

fn contract_error(message: impl Into<String>) -> BroadcastEngineError {
    BroadcastEngineError::new(BroadcastEngineErrorKind::Contract, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qnc_broadcast_player::{
        AudioFormat, AudioFramePacket, ColorSpace, DecodedVideoFrame, EngineFrameRequest,
        EngineSourceHandle, FieldMode, FramePresenter, FrameRange, SourceOpenAdapter, Timebase,
        TransportEngine, TransportStatus, VideoDecodeAdapter, VideoFormat,
    };

    #[test]
    fn runtime_drives_transport_from_protocol_command() {
        let mut runtime = fake_runtime();
        let request =
            qnc_broadcast_player::BroadcastPlaybackRequest::new("request-1", source_runtime("src"))
                .unwrap()
                .with_range(FrameRange::new(10, 12).unwrap())
                .unwrap();

        runtime.dispatch_at(
            PlayerRuntimeCommand::new(
                "cmd-request",
                BroadcastPlayerProtocolCommand::SetPlaybackRequest {
                    request: Box::new(request),
                },
            ),
            0,
        );
        runtime.dispatch_at(
            PlayerRuntimeCommand::new("cmd-play", BroadcastPlayerProtocolCommand::Play),
            0,
        );
        runtime.tick(0);
        runtime.tick(40_000_000);
        runtime.tick(80_000_000);

        let events = runtime.drain_events();

        assert!(events.iter().any(|event| matches!(
            event,
            BroadcastPlayerProtocolEvent::CarrierPositionChanged { frame: 10, .. }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            BroadcastPlayerProtocolEvent::FramePresented { frame: 12 }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            BroadcastPlayerProtocolEvent::PlaybackBoundaryReached { frame: 12 }
        )));
        assert_eq!(runtime.transport().state().carrier_frame, 12);
        assert_eq!(runtime.transport().state().status, TransportStatus::Paused);
    }

    #[test]
    fn runtime_uses_registered_preloaded_source_snapshot() {
        let mut runtime = fake_runtime();
        runtime.dispatch_at(
            PlayerRuntimeCommand::new(
                "cmd-preload",
                BroadcastPlayerProtocolCommand::PreloadSource {
                    source: source_runtime("src-b"),
                },
            ),
            0,
        );

        runtime.dispatch_at(
            PlayerRuntimeCommand::new(
                "cmd-active",
                BroadcastPlayerProtocolCommand::SetActiveSource {
                    source_id: "src-b".to_string(),
                },
            ),
            0,
        );

        let events = runtime.drain_events();

        assert!(events.iter().any(|event| matches!(
            event,
            BroadcastPlayerProtocolEvent::SourcePreloaded { source_id } if source_id == "src-b"
        )));
        assert_eq!(
            runtime
                .transport()
                .state()
                .source
                .as_ref()
                .unwrap()
                .source_id,
            "src-b"
        );
    }

    #[test]
    fn runtime_rejects_unknown_active_source_without_changing_transport() {
        let mut runtime = fake_runtime();

        runtime.dispatch_at(
            PlayerRuntimeCommand::new(
                "cmd-active",
                BroadcastPlayerProtocolCommand::SetActiveSource {
                    source_id: "missing".to_string(),
                },
            ),
            0,
        );

        let events = runtime.drain_events();

        assert!(matches!(
            events.as_slice(),
            [
                BroadcastPlayerProtocolEvent::CommandRejected { command_name, .. },
                BroadcastPlayerProtocolEvent::PlaybackError { .. }
            ] if command_name == "SetActiveSource"
        ));
        assert!(runtime.transport().state().source.is_none());
    }

    #[test]
    fn runtime_event_queue_is_bounded_and_keeps_latest_events() {
        let mut runtime = fake_runtime_with_limit(3);
        runtime.dispatch_at(
            PlayerRuntimeCommand::new(
                "cmd-request",
                BroadcastPlayerProtocolCommand::SetPlaybackRequest {
                    request: Box::new(
                        qnc_broadcast_player::BroadcastPlaybackRequest::new(
                            "request-1",
                            source_runtime("src"),
                        )
                        .unwrap(),
                    ),
                },
            ),
            0,
        );
        let _ = runtime.drain_events();

        for index in 0..5 {
            runtime.dispatch_at(
                PlayerRuntimeCommand::new(
                    format!("cmd-play-{index}"),
                    BroadcastPlayerProtocolCommand::Play,
                ),
                0,
            );
        }

        let events = runtime.drain_events();

        assert_eq!(runtime.event_queue_limit(), 3);
        assert_eq!(events.len(), 3);
        assert!(events.iter().all(|event| {
            !matches!(
                event,
                BroadcastPlayerProtocolEvent::CommandAccepted { command_id, .. }
                    if command_id == "cmd-play-0"
            )
        }));
        assert!(events.iter().any(|event| matches!(
            event,
            BroadcastPlayerProtocolEvent::CommandAccepted { command_id, .. }
                if command_id == "cmd-play-4"
        )));
        assert!(matches!(
            events.last(),
            Some(BroadcastPlayerProtocolEvent::TransportStatusChanged {
                status: TransportStatus::Playing
            })
        ));
    }

    fn fake_runtime() -> BroadcastPlayerRuntime<
        TransportEngine<FakeSourceOpen, FakeVideoDecode, FakeAudioOutput, FakePresenter>,
    > {
        BroadcastPlayerRuntime::new(TransportEngine::new(
            FakeSourceOpen,
            FakeVideoDecode,
            FakeAudioOutput,
            FakePresenter,
        ))
    }

    fn fake_runtime_with_limit(
        event_queue_limit: usize,
    ) -> BroadcastPlayerRuntime<
        TransportEngine<FakeSourceOpen, FakeVideoDecode, FakeAudioOutput, FakePresenter>,
    > {
        BroadcastPlayerRuntime::with_event_queue_limit(
            TransportEngine::new(
                FakeSourceOpen,
                FakeVideoDecode,
                FakeAudioOutput,
                FakePresenter,
            ),
            event_queue_limit,
        )
    }

    fn source_runtime(source_id: &str) -> SourceRuntime {
        SourceRuntime::new(source_id, 20, Timebase::new(25, 1).unwrap())
            .unwrap()
            .with_video_format(
                VideoFormat::new(160, 90, FieldMode::Progressive, ColorSpace::Rec709).unwrap(),
            )
            .with_audio_format(AudioFormat::new(48_000, 1).unwrap())
    }

    struct FakeSourceOpen;

    impl SourceOpenAdapter for FakeSourceOpen {
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

    struct FakeVideoDecode;

    impl VideoDecodeAdapter for FakeVideoDecode {
        type VideoFrame = FrameNumber;

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
                payload: request.frame,
            })
        }
    }

    struct FakeAudioOutput;

    impl qnc_broadcast_player::AudioOutputAdapter for FakeAudioOutput {
        type AudioPacket = FrameNumber;

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
                payload: request.frame,
            })
        }

        fn submit_audio_packet(
            &mut self,
            _packet: AudioFramePacket<Self::AudioPacket>,
        ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
            Ok(Vec::new())
        }

        fn stop_audio(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
            Ok(Vec::new())
        }
    }

    struct FakePresenter;

    impl FramePresenter for FakePresenter {
        type VideoFrame = FrameNumber;

        fn present_frame(
            &mut self,
            frame: DecodedVideoFrame<Self::VideoFrame>,
        ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
            Ok(vec![BroadcastEvent::FramePresented { frame: frame.frame }])
        }
    }
}
