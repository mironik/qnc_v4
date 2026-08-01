use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex};

use qnc_broadcast_player::{
    BroadcastEngineError, BroadcastEngineErrorKind, BroadcastEvent, BroadcastPlayerProtocolEvent,
    DecodedVideoFrame, FrameNumber, FramePresenter,
};
use qnc_player_monitor::{MonitorError, MonitorPixelLayout};
pub use qnc_player_monitor::{MonitorFrameBuffer, PlayerMonitor, PlayerMonitorState};

#[derive(Clone, Debug, Default)]
pub struct SharedPlayerMonitor {
    inner: Arc<Mutex<PlayerMonitor>>,
}

impl SharedPlayerMonitor {
    pub fn new(monitor: PlayerMonitor) -> Self {
        Self {
            inner: Arc::new(Mutex::new(monitor)),
        }
    }

    pub fn snapshot(&self) -> Result<PlayerMonitorState, MonitorBridgeError> {
        let monitor = self.lock_monitor()?;
        Ok(monitor.state().clone())
    }

    pub fn apply_event(
        &self,
        event: &BroadcastPlayerProtocolEvent,
    ) -> Result<(), MonitorBridgeError> {
        let mut monitor = self.lock_monitor()?;
        monitor.apply_event(event);
        Ok(())
    }

    pub fn accept_frame_buffer(
        &self,
        frame_buffer: MonitorFrameBuffer,
    ) -> Result<(), MonitorBridgeError> {
        let mut monitor = self.lock_monitor()?;
        monitor.accept_frame_buffer(frame_buffer)?;
        Ok(())
    }

    fn lock_monitor(&self) -> Result<std::sync::MutexGuard<'_, PlayerMonitor>, MonitorBridgeError> {
        self.inner
            .lock()
            .map_err(|_| MonitorBridgeError::MonitorLockUnavailable)
    }
}

#[derive(Clone, Debug)]
pub struct MonitorEventBridge {
    monitor: SharedPlayerMonitor,
}

impl MonitorEventBridge {
    pub fn new(monitor: SharedPlayerMonitor) -> Self {
        Self { monitor }
    }

    pub fn monitor(&self) -> &SharedPlayerMonitor {
        &self.monitor
    }

    pub fn apply_event(
        &self,
        event: &BroadcastPlayerProtocolEvent,
    ) -> Result<(), MonitorBridgeError> {
        self.monitor.apply_event(event)
    }

    pub fn apply_events(
        &self,
        events: &[BroadcastPlayerProtocolEvent],
    ) -> Result<(), MonitorBridgeError> {
        for event in events {
            self.apply_event(event)?;
        }
        Ok(())
    }
}

pub trait MonitorFrameMapper<T> {
    fn map_frame(
        &mut self,
        frame: &DecodedVideoFrame<T>,
    ) -> Result<MonitorFrameBuffer, MonitorBridgeError>;
}

impl<T, F> MonitorFrameMapper<T> for F
where
    F: FnMut(&DecodedVideoFrame<T>) -> Result<MonitorFrameBuffer, MonitorBridgeError>,
{
    fn map_frame(
        &mut self,
        frame: &DecodedVideoFrame<T>,
    ) -> Result<MonitorFrameBuffer, MonitorBridgeError> {
        self(frame)
    }
}

#[derive(Clone, Debug)]
pub struct MonitorFramePresenter<P, M> {
    inner: P,
    monitor: SharedPlayerMonitor,
    mapper: M,
}

impl<P, M> MonitorFramePresenter<P, M> {
    pub fn new(inner: P, monitor: SharedPlayerMonitor, mapper: M) -> Self {
        Self {
            inner,
            monitor,
            mapper,
        }
    }

    pub fn monitor(&self) -> &SharedPlayerMonitor {
        &self.monitor
    }

    pub fn inner(&self) -> &P {
        &self.inner
    }

    pub fn into_inner(self) -> P {
        self.inner
    }
}

impl<P, M> FramePresenter for MonitorFramePresenter<P, M>
where
    P: FramePresenter,
    M: MonitorFrameMapper<P::VideoFrame>,
{
    type VideoFrame = P::VideoFrame;

    fn present_frame(
        &mut self,
        frame: DecodedVideoFrame<Self::VideoFrame>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let frame_buffer = self
            .mapper
            .map_frame(&frame)
            .map_err(|err| bridge_engine_error(frame.source_id.clone(), frame.frame, err))?;
        let monitor_source_id = frame_buffer.source_id.clone();
        let monitor_frame = frame_buffer.frame;
        let events = self.inner.present_frame(frame)?;
        self.monitor
            .accept_frame_buffer(frame_buffer)
            .map_err(|err| {
                bridge_engine_error(optional_source_id(monitor_source_id), monitor_frame, err)
            })?;
        Ok(events)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonitorBridgeError {
    Monitor(MonitorError),
    MonitorLockUnavailable,
    FrameBufferUnavailable(String),
}

impl fmt::Display for MonitorBridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Monitor(error) => write!(formatter, "{error}"),
            Self::MonitorLockUnavailable => write!(formatter, "monitor lock unavailable"),
            Self::FrameBufferUnavailable(message) => write!(formatter, "{message}"),
        }
    }
}

impl Error for MonitorBridgeError {}

impl From<MonitorError> for MonitorBridgeError {
    fn from(error: MonitorError) -> Self {
        Self::Monitor(error)
    }
}

fn bridge_engine_error(
    source_id: String,
    frame: FrameNumber,
    error: MonitorBridgeError,
) -> BroadcastEngineError {
    BroadcastEngineError::new(BroadcastEngineErrorKind::VideoPresent, error.to_string())
        .with_source_id(source_id)
        .with_frame(frame)
}

fn optional_source_id(source_id: Option<String>) -> String {
    source_id.unwrap_or_else(|| "unknown".to_string())
}

pub fn rgb24_monitor_frame_buffer(
    source_id: String,
    frame: FrameNumber,
    video_format: qnc_broadcast_player::VideoFormat,
    bytes: Vec<u8>,
) -> Result<MonitorFrameBuffer, MonitorBridgeError> {
    MonitorFrameBuffer::new(
        Some(source_id),
        frame,
        video_format,
        MonitorPixelLayout::Rgb24,
        bytes,
    )
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qnc_broadcast_player::{
        ColorSpace, FieldMode, PixelAspect, Timebase, TransportStatus, VideoFormat,
    };

    #[test]
    fn event_bridge_projects_runtime_events_into_monitor() {
        let shared = SharedPlayerMonitor::default();
        let bridge = MonitorEventBridge::new(shared.clone());

        bridge
            .apply_events(&[
                BroadcastPlayerProtocolEvent::TransportStatusChanged {
                    status: TransportStatus::Playing,
                },
                BroadcastPlayerProtocolEvent::CarrierPositionChanged {
                    source_id: Some("src-a".to_string()),
                    frame: 14,
                    range: None,
                    timebase: Some(Timebase::new(25, 1).unwrap()),
                    status: TransportStatus::Playing,
                },
            ])
            .unwrap();

        let snapshot = shared.snapshot().unwrap();
        assert_eq!(snapshot.active_source_id.as_deref(), Some("src-a"));
        assert_eq!(snapshot.carrier_frame, Some(14));
        assert_eq!(snapshot.transport_status, TransportStatus::Playing);
        assert_eq!(snapshot.event_revision, 2);
    }

    #[test]
    fn frame_presenter_updates_monitor_after_inner_presenter_accepts_frame() {
        let shared = SharedPlayerMonitor::default();
        let mapper = StaticMapper::new(video_format());
        let mut presenter = MonitorFramePresenter::new(AcceptingPresenter, shared.clone(), mapper);

        let events = presenter
            .present_frame(decoded_frame(4, vec![1; 12]))
            .unwrap();

        let snapshot = shared.snapshot().unwrap();
        assert!(matches!(
            events.as_slice(),
            [BroadcastEvent::FramePresented { frame: 4 }]
        ));
        assert_eq!(snapshot.presented_frame, Some(4));
        assert_eq!(snapshot.frame_revision, 1);
        assert_eq!(
            snapshot.last_frame_buffer.as_ref().unwrap().bytes,
            vec![1; 12]
        );
    }

    #[test]
    fn frame_presenter_does_not_update_monitor_when_inner_presenter_rejects_frame() {
        let shared = SharedPlayerMonitor::default();
        let mapper = StaticMapper::new(video_format());
        let mut presenter = MonitorFramePresenter::new(RejectingPresenter, shared.clone(), mapper);

        let error = presenter
            .present_frame(decoded_frame(4, vec![1; 12]))
            .unwrap_err();

        assert_eq!(error.kind, BroadcastEngineErrorKind::VideoPresent);
        assert_eq!(shared.snapshot().unwrap().last_frame_buffer, None);
    }

    #[test]
    fn frame_presenter_maps_invalid_buffer_to_engine_error() {
        let shared = SharedPlayerMonitor::default();
        let mapper = StaticMapper::new(video_format());
        let mut presenter = MonitorFramePresenter::new(AcceptingPresenter, shared.clone(), mapper);

        let error = presenter
            .present_frame(decoded_frame(4, vec![1; 11]))
            .unwrap_err();

        assert_eq!(error.kind, BroadcastEngineErrorKind::VideoPresent);
        assert_eq!(error.source_id.as_deref(), Some("src-a"));
        assert_eq!(error.frame, Some(4));
        assert_eq!(shared.snapshot().unwrap().last_frame_buffer, None);
    }

    #[derive(Clone, Debug)]
    struct StaticMapper {
        video_format: VideoFormat,
    }

    impl StaticMapper {
        fn new(video_format: VideoFormat) -> Self {
            Self { video_format }
        }
    }

    impl MonitorFrameMapper<Vec<u8>> for StaticMapper {
        fn map_frame(
            &mut self,
            frame: &DecodedVideoFrame<Vec<u8>>,
        ) -> Result<MonitorFrameBuffer, MonitorBridgeError> {
            rgb24_monitor_frame_buffer(
                frame.source_id.clone(),
                frame.frame,
                self.video_format.clone(),
                frame.payload.clone(),
            )
        }
    }

    struct AcceptingPresenter;

    impl FramePresenter for AcceptingPresenter {
        type VideoFrame = Vec<u8>;

        fn present_frame(
            &mut self,
            frame: DecodedVideoFrame<Self::VideoFrame>,
        ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
            Ok(vec![BroadcastEvent::FramePresented { frame: frame.frame }])
        }
    }

    struct RejectingPresenter;

    impl FramePresenter for RejectingPresenter {
        type VideoFrame = Vec<u8>;

        fn present_frame(
            &mut self,
            frame: DecodedVideoFrame<Self::VideoFrame>,
        ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
            Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::VideoPresent,
                "presenter rejected frame",
            )
            .with_source_id(frame.source_id)
            .with_frame(frame.frame))
        }
    }

    fn decoded_frame(frame: FrameNumber, payload: Vec<u8>) -> DecodedVideoFrame<Vec<u8>> {
        DecodedVideoFrame {
            source_id: "src-a".to_string(),
            frame,
            video_format: Some(video_format()),
            payload,
        }
    }

    fn video_format() -> VideoFormat {
        VideoFormat {
            width: 2,
            height: 2,
            field_mode: FieldMode::Progressive,
            color_space: ColorSpace::Rec709,
            pixel_aspect: PixelAspect::square(),
        }
    }
}
