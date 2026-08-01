use qnc_broadcast_player::model::FrameNumber;
use qnc_broadcast_player::{
    AudioOutputAdapter, BroadcastEngineError, BroadcastEvent, ClockTick, FramePresenter,
    FrameRange, SourceOpenAdapter, SourceRuntime, TransportEngine, TransportEngineState,
    VideoDecodeAdapter,
};

pub trait PlayerTransport {
    fn load_source(
        &mut self,
        source: &SourceRuntime,
        source_revision: Option<u64>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError>;

    fn preload_source(
        &mut self,
        source: &SourceRuntime,
        source_revision: Option<u64>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError>;

    fn set_active_source(
        &mut self,
        source: &SourceRuntime,
        source_revision: Option<u64>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError>;

    fn unload_source(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError>;

    fn play(&mut self, now_tick: ClockTick) -> Result<Vec<BroadcastEvent>, BroadcastEngineError>;

    fn pause(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError>;

    fn stop(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError>;

    fn apply_request_rate(
        &mut self,
        rate_num: i32,
        rate_den: u32,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError>;

    fn sync_range_runtime(
        &mut self,
        active_range: Option<FrameRange>,
        carrier_frame: FrameNumber,
        present_frame: bool,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError>;

    fn tick(&mut self, now_tick: ClockTick) -> Result<Vec<BroadcastEvent>, BroadcastEngineError>;

    fn state(&self) -> &TransportEngineState;
}

impl<S, V, A, P> PlayerTransport for TransportEngine<S, V, A, P>
where
    S: SourceOpenAdapter,
    V: VideoDecodeAdapter,
    A: AudioOutputAdapter,
    P: FramePresenter<VideoFrame = V::VideoFrame>,
{
    fn load_source(
        &mut self,
        source: &SourceRuntime,
        source_revision: Option<u64>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        TransportEngine::load_source(self, source, source_revision)
    }

    fn preload_source(
        &mut self,
        source: &SourceRuntime,
        source_revision: Option<u64>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        TransportEngine::preload_source(self, source, source_revision)
    }

    fn set_active_source(
        &mut self,
        source: &SourceRuntime,
        source_revision: Option<u64>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        TransportEngine::set_active_source(self, source, source_revision)
    }

    fn unload_source(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        TransportEngine::unload_source(self)
    }

    fn play(&mut self, now_tick: ClockTick) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        TransportEngine::play(self, now_tick)
    }

    fn pause(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        TransportEngine::pause(self)
    }

    fn stop(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        TransportEngine::stop(self)
    }

    fn apply_request_rate(
        &mut self,
        rate_num: i32,
        rate_den: u32,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        TransportEngine::apply_request_rate(self, rate_num, rate_den)
    }

    fn sync_range_runtime(
        &mut self,
        active_range: Option<FrameRange>,
        carrier_frame: FrameNumber,
        present_frame: bool,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        TransportEngine::sync_range_runtime(self, active_range, carrier_frame, present_frame)
    }

    fn tick(&mut self, now_tick: ClockTick) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        TransportEngine::tick(self, now_tick)
    }

    fn state(&self) -> &TransportEngineState {
        TransportEngine::state(self)
    }
}
