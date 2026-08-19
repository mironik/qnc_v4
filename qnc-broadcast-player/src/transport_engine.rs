use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::engine_contract::{
    AudioFramePacket, AudioOutputAdapter, BroadcastEngineError, BroadcastEngineErrorKind,
    DecodedVideoFrame, EngineFrameRequest, EngineSourceHandle, FramePresenter, SourceOpenAdapter,
    VideoDecodeAdapter,
};
use crate::event::BroadcastEvent;
use crate::frame_clock::{ClockTick, FrameClock, FrameClockConfig, FrameClockRate};
use crate::model::{FrameNumber, FrameRange, SourceRuntime, TransportStatus};

const DEFAULT_DECODE_BURST_FRAMES: usize = 4;
const MIN_PLAYOUT_BUFFER_FRAMES: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportEngineState {
    pub source: Option<EngineSourceHandle>,
    pub preloaded_sources: BTreeMap<String, EngineSourceHandle>,
    pub carrier_frame: FrameNumber,
    pub status: TransportStatus,
    pub active_range: Option<FrameRange>,
    pub playback_rate_num: i32,
    pub playback_rate_den: u32,
    pub decode_burst_frames: usize,
}

impl Default for TransportEngineState {
    fn default() -> Self {
        Self {
            source: None,
            preloaded_sources: BTreeMap::new(),
            carrier_frame: 0,
            status: TransportStatus::Empty,
            active_range: None,
            playback_rate_num: 1,
            playback_rate_den: 1,
            decode_burst_frames: DEFAULT_DECODE_BURST_FRAMES,
        }
    }
}

pub struct TransportEngine<S, V, A, P>
where
    S: SourceOpenAdapter,
    V: VideoDecodeAdapter,
    A: AudioOutputAdapter,
    P: FramePresenter<VideoFrame = V::VideoFrame>,
{
    source_open: S,
    video_decode: V,
    audio_output: A,
    frame_presenter: P,
    clock: Option<FrameClock>,
    state: TransportEngineState,
    playout: PlayoutBuffer<V::VideoFrame, A::AudioPacket>,
}

impl<S, V, A, P> TransportEngine<S, V, A, P>
where
    S: SourceOpenAdapter,
    V: VideoDecodeAdapter,
    A: AudioOutputAdapter,
    P: FramePresenter<VideoFrame = V::VideoFrame>,
{
    pub fn new(source_open: S, video_decode: V, audio_output: A, frame_presenter: P) -> Self {
        Self {
            source_open,
            video_decode,
            audio_output,
            frame_presenter,
            clock: None,
            state: TransportEngineState::default(),
            playout: PlayoutBuffer::default(),
        }
    }

    pub fn with_decode_burst_frames(mut self, decode_burst_frames: usize) -> Self {
        self.state.decode_burst_frames = decode_burst_frames.max(1);
        self
    }

    pub fn state(&self) -> &TransportEngineState {
        &self.state
    }

    pub fn load_source(
        &mut self,
        source: &SourceRuntime,
        source_revision: Option<u64>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let handle = self.source_open.open_source(source, source_revision)?;
        let mut prepared_events = self.prepare_source_handle(&handle)?;

        let mut events = self.interrupt_motion()?;
        self.close_active_source()?;
        self.close_preloaded_source(&source.source_id)?;
        events.append(&mut prepared_events);
        events.extend(self.activate_source_handle(handle));
        Ok(events)
    }

    pub fn preload_source(
        &mut self,
        source: &SourceRuntime,
        source_revision: Option<u64>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let handle = self.source_open.open_source(source, source_revision)?;
        let mut events = self.prepare_source_handle(&handle)?;
        self.close_preloaded_source(&source.source_id)?;
        let source_id = handle.source_id.clone();
        self.state
            .preloaded_sources
            .insert(source_id.clone(), handle);
        events.push(BroadcastEvent::SourcePreloaded { source_id });
        Ok(events)
    }

    pub fn set_active_source(
        &mut self,
        source: &SourceRuntime,
        source_revision: Option<u64>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        if let Some(handle) = self.state.preloaded_sources.remove(&source.source_id) {
            let mut events = self.interrupt_motion()?;
            self.close_active_source()?;
            events.extend(self.activate_source_handle(handle));
            return Ok(events);
        }
        self.load_source(source, source_revision)
    }

    pub fn unload_source(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let mut events = self.interrupt_motion()?;
        self.close_active_source()?;
        self.close_preloaded_sources()?;
        self.playout.clear();
        self.state.carrier_frame = 0;
        self.state.status = TransportStatus::Empty;
        self.state.active_range = None;
        events.push(BroadcastEvent::ActiveSourceChanged { source_id: None });
        events.push(BroadcastEvent::RangeChanged { range: None });
        events.push(BroadcastEvent::TransportStatusChanged {
            status: TransportStatus::Empty,
        });
        events.push(self.position_event());
        Ok(events)
    }

    pub fn play(
        &mut self,
        now_tick: ClockTick,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let (range, timebase) = {
            let source = self.require_source()?;
            (self.active_range_or_source(source)?, source.timebase)
        };
        if self.state.carrier_frame >= range.end_frame {
            self.state.carrier_frame = range.start_frame;
        }
        let rate = FrameClockRate::new(self.state.playback_rate_num, self.state.playback_rate_den)
            .map_err(|err| BroadcastEngineError::new(BroadcastEngineErrorKind::Contract, err))?;
        self.clock = Some(FrameClock::start(
            FrameClockConfig::new(timebase, rate),
            self.state.carrier_frame,
            now_tick,
        ));
        self.state.status = TransportStatus::Playing;
        let mut events = Vec::new();
        events.push(BroadcastEvent::TransportStatusChanged {
            status: TransportStatus::Playing,
        });
        Ok(events)
    }

    pub fn pause(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let mut events = self.interrupt_motion()?;
        self.require_source()?;
        self.state.status = TransportStatus::Paused;
        events.push(BroadcastEvent::TransportStatusChanged {
            status: TransportStatus::Paused,
        });
        Ok(events)
    }

    pub fn stop(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let mut events = self.interrupt_motion()?;
        self.require_source()?;
        self.state.status = TransportStatus::Stopped;
        events.push(BroadcastEvent::TransportStatusChanged {
            status: TransportStatus::Stopped,
        });
        Ok(events)
    }

    pub fn sync_range_runtime(
        &mut self,
        active_range: Option<FrameRange>,
        carrier_frame: FrameNumber,
        present_frame: bool,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let mut events = self.interrupt_motion()?;
        let was_moving = matches!(self.state.status, TransportStatus::Playing);
        let resolved_range = {
            let source = self.require_source()?;
            let range = active_range.unwrap_or(self.active_range_or_source(source)?);
            self.require_range_inside_source(range)?;
            range
        };
        let carrier_changed = self.state.carrier_frame != carrier_frame;

        self.state.active_range = Some(resolved_range);
        self.require_frame_in_active_range(carrier_frame)?;
        self.state.carrier_frame = carrier_frame;
        self.playout.reset_for_frame(carrier_frame);

        events.push(BroadcastEvent::RangeChanged {
            range: Some(resolved_range),
        });

        if was_moving || carrier_changed || present_frame {
            self.state.status = TransportStatus::Paused;
            events.push(BroadcastEvent::TransportStatusChanged {
                status: TransportStatus::Paused,
            });
        }

        if carrier_changed || present_frame {
            events.extend(self.present_current_frame()?);
        } else {
            events.push(self.position_event());
        }
        Ok(events)
    }

    pub fn apply_request_rate(
        &mut self,
        rate_num: i32,
        rate_den: u32,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let mut events = self.interrupt_motion()?;
        self.require_source()?;
        FrameClockRate::new(rate_num, rate_den)
            .map_err(|err| BroadcastEngineError::new(BroadcastEngineErrorKind::Contract, err))?;
        self.state.playback_rate_num = rate_num;
        self.state.playback_rate_den = rate_den;
        self.state.status = TransportStatus::Ready;
        events.push(BroadcastEvent::TransportStatusChanged {
            status: TransportStatus::Ready,
        });
        Ok(events)
    }

    pub fn tick(
        &mut self,
        now_tick: ClockTick,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        if self.clock.is_none() {
            return Ok(Vec::new());
        };
        let mut events = Vec::new();
        let max_due = self.state.decode_burst_frames.max(1);
        for _ in 0..max_due {
            let Some((scheduled, advanced_clock)) = self.peek_next_due_frame(now_tick) else {
                break;
            };

            if self.frame_outside_source(scheduled.frame)? {
                events.extend(self.stop()?);
                return Ok(events);
            }

            let range = self.current_range()?;
            let frame = scheduled.frame.min(range.end_frame);
            if !self.playout_ready(frame)? {
                self.playout.reset_for_frame(frame);
                events.extend(self.refill_playout_buffer(frame, self.underrun_recover_frames())?);
                if !self.playout_ready(frame)? {
                    return Ok(events);
                }
            }

            self.clock = Some(advanced_clock);
            events.extend(self.present_buffered_frame(frame)?);
            if frame >= range.end_frame {
                events.extend(self.apply_playback_boundary()?);
                return Ok(events);
            }
        }
        events.extend(self.refill_from_current_position()?);
        Ok(events)
    }

    fn peek_next_due_frame(
        &self,
        now_tick: ClockTick,
    ) -> Option<(crate::ScheduledFrame, FrameClock)> {
        let mut clock = self.clock.clone()?;
        let scheduled = clock.next_due_frame(now_tick)?;
        Some((scheduled, clock))
    }

    fn apply_playback_boundary(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let range = self.current_range()?;
        let mut events = vec![BroadcastEvent::PlaybackBoundaryReached {
            frame: range.end_frame,
        }];
        events.extend(self.interrupt_motion()?);
        self.state.status = TransportStatus::Paused;
        events.push(BroadcastEvent::TransportStatusChanged {
            status: TransportStatus::Paused,
        });
        Ok(events)
    }

    fn interrupt_motion(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let events = self.audio_output.stop_audio()?;
        self.clock = None;
        Ok(events)
    }

    fn prepare_source_handle(
        &mut self,
        handle: &EngineSourceHandle,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        if handle.video_format.is_none() && handle.audio_format.is_none() {
            let _ = self.source_open.close_source(&handle.source_id);
            return Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::Contract,
                "source has no playable tracks",
            )
            .with_source_id(handle.source_id.clone()));
        }

        let mut events = Vec::new();
        if handle.video_format.is_some() {
            match self.video_decode.prepare_video(handle) {
                Ok(video_events) => events.extend(video_events),
                Err(error) => {
                    let _ = self.source_open.close_source(&handle.source_id);
                    return Err(error);
                }
            }
        }
        if handle.audio_format.is_some() {
            match self.audio_output.prepare_audio(handle) {
                Ok(audio_events) => events.extend(audio_events),
                Err(error) => {
                    let _ = self.source_open.close_source(&handle.source_id);
                    return Err(error);
                }
            }
        }
        Ok(events)
    }

    fn activate_source_handle(&mut self, handle: EngineSourceHandle) -> Vec<BroadcastEvent> {
        self.state.source = Some(handle.clone());
        self.state.carrier_frame = 0;
        self.state.status = TransportStatus::Ready;
        self.state.active_range = source_range(&handle);
        self.state.playback_rate_num = 1;
        self.state.playback_rate_den = 1;
        self.playout.reset_for_frame(0);
        vec![
            BroadcastEvent::SourceReady {
                source_id: handle.source_id,
            },
            BroadcastEvent::RangeChanged {
                range: self.state.active_range,
            },
            BroadcastEvent::TransportStatusChanged {
                status: TransportStatus::Ready,
            },
            self.position_event(),
        ]
    }

    fn close_active_source(&mut self) -> Result<(), BroadcastEngineError> {
        if let Some(active_source) = self.state.source.take() {
            self.source_open.close_source(&active_source.source_id)?;
        }
        self.playout.clear();
        Ok(())
    }

    fn close_preloaded_source(&mut self, source_id: &str) -> Result<(), BroadcastEngineError> {
        if let Some(source) = self.state.preloaded_sources.remove(source_id) {
            self.source_open.close_source(&source.source_id)?;
        }
        Ok(())
    }

    fn close_preloaded_sources(&mut self) -> Result<(), BroadcastEngineError> {
        let source_ids: Vec<String> = self.state.preloaded_sources.keys().cloned().collect();
        for source_id in source_ids {
            self.close_preloaded_source(&source_id)?;
        }
        Ok(())
    }

    fn present_current_frame(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        self.decode_frame_to_buffer(self.state.carrier_frame)?;
        self.present_buffered_frame(self.state.carrier_frame)
    }

    fn decode_frame_to_buffer(&mut self, frame: FrameNumber) -> Result<(), BroadcastEngineError> {
        let source = self.require_source()?.clone();
        let request = EngineFrameRequest::new(&source, frame)?;
        if source.video_format.is_some() && !self.playout.video.contains_key(&frame) {
            let video_frame = self.video_decode.decode_video_frame(request.clone())?;
            self.playout.video.insert(frame, video_frame);
        }
        if source.audio_format.is_some() && !self.playout.audio.contains_key(&frame) {
            let audio_packet = self.audio_output.render_audio_for_frame(request)?;
            self.playout.audio.insert(frame, audio_packet);
        }
        Ok(())
    }

    fn present_buffered_frame(
        &mut self,
        frame: FrameNumber,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let source = self.require_source()?.clone();
        self.state.carrier_frame = frame;
        let mut events = vec![self.position_event()];
        if source.audio_format.is_some() {
            let audio_packet = self.playout.audio.remove(&frame).ok_or_else(|| {
                BroadcastEngineError::new(
                    BroadcastEngineErrorKind::AudioOutput,
                    format!("audio frame {frame} is not ready for playout"),
                )
                .with_source_id(source.source_id.clone())
                .with_frame(frame)
            })?;
            events.extend(self.audio_output.submit_audio_packet(audio_packet)?);
        }
        if source.video_format.is_some() {
            let video_frame = self.playout.video.remove(&frame).ok_or_else(|| {
                BroadcastEngineError::new(
                    BroadcastEngineErrorKind::VideoDecode,
                    format!("video frame {frame} is not ready for playout"),
                )
                .with_source_id(source.source_id.clone())
                .with_frame(frame)
            })?;
            events.extend(self.frame_presenter.present_frame(video_frame)?);
        }
        self.playout.trim_before(frame.saturating_sub(1));
        Ok(events)
    }

    fn refill_from_current_position(
        &mut self,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let ahead = self.buffered_ahead_from(self.state.carrier_frame)?;
        let budget = refill_budget(
            matches!(self.state.status, TransportStatus::Playing),
            ahead,
            self.healthy_buffer_frames(),
            self.state.decode_burst_frames,
        );
        self.refill_playout_buffer(self.state.carrier_frame, budget)
    }

    fn refill_playout_buffer(
        &mut self,
        anchor_frame: FrameNumber,
        max_frames: usize,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let range = self.current_range()?;
        let mut frame = self
            .playout
            .next_decode_frame
            .filter(|frame| *frame >= anchor_frame)
            .unwrap_or(anchor_frame);
        let mut decoded = 0;
        while decoded < max_frames && frame <= range.end_frame {
            self.decode_frame_to_buffer(frame)?;
            decoded += 1;
            frame = frame.saturating_add(1);
            self.playout.next_decode_frame = Some(frame);
        }
        Ok(Vec::new())
    }

    fn playout_ready(&self, frame: FrameNumber) -> Result<bool, BroadcastEngineError> {
        let source = self.require_source()?;
        Ok(self.playout.has_ready_frame(source, frame))
    }

    fn buffered_ahead_from(&self, frame: FrameNumber) -> Result<usize, BroadcastEngineError> {
        let source = self.require_source()?;
        Ok(self
            .playout
            .newest_ready_frame(source)
            .and_then(|newest| newest.checked_sub(frame))
            .and_then(|ahead| usize::try_from(ahead).ok())
            .unwrap_or(0))
    }

    fn healthy_buffer_frames(&self) -> usize {
        self.state
            .decode_burst_frames
            .saturating_mul(4)
            .max(MIN_PLAYOUT_BUFFER_FRAMES)
    }

    fn underrun_recover_frames(&self) -> usize {
        self.state
            .decode_burst_frames
            .max(MIN_PLAYOUT_BUFFER_FRAMES)
    }

    fn position_event(&self) -> BroadcastEvent {
        let source = self.state.source.as_ref();
        BroadcastEvent::CarrierPositionChanged {
            source_id: source.map(|source| source.source_id.clone()),
            frame: self.state.carrier_frame,
            range: self
                .state
                .active_range
                .or_else(|| source.and_then(source_range)),
            timebase: source.map(|source| source.timebase),
            status: self.state.status,
        }
    }

    fn require_source(&self) -> Result<&EngineSourceHandle, BroadcastEngineError> {
        self.state.source.as_ref().ok_or_else(|| {
            BroadcastEngineError::new(BroadcastEngineErrorKind::Contract, "source not loaded")
        })
    }

    fn frame_outside_source(&self, frame: FrameNumber) -> Result<bool, BroadcastEngineError> {
        Ok(frame > self.require_source()?.duration_frames)
    }

    fn require_frame_in_active_range(
        &self,
        frame: FrameNumber,
    ) -> Result<(), BroadcastEngineError> {
        let range = self.current_range()?;
        if !range.contains_position(frame) {
            return Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::Contract,
                format!(
                    "frame {frame} is outside active range {}..{}",
                    range.start_frame, range.end_frame
                ),
            )
            .with_frame(frame));
        }
        Ok(())
    }

    fn require_range_inside_source(&self, range: FrameRange) -> Result<(), BroadcastEngineError> {
        let source = self.require_source()?;
        if range.end_frame > source.duration_frames {
            return Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::Contract,
                format!(
                    "range end {} is outside source duration {}",
                    range.end_frame, source.duration_frames
                ),
            ));
        }
        Ok(())
    }

    fn current_range(&self) -> Result<FrameRange, BroadcastEngineError> {
        let source = self.require_source()?;
        self.active_range_or_source(source)
    }

    fn active_range_or_source(
        &self,
        source: &EngineSourceHandle,
    ) -> Result<FrameRange, BroadcastEngineError> {
        self.state
            .active_range
            .or_else(|| source_range(source))
            .ok_or_else(|| {
                BroadcastEngineError::new(
                    BroadcastEngineErrorKind::Contract,
                    "source duration cannot define active range",
                )
            })
    }
}

struct PlayoutBuffer<V, A> {
    video: BTreeMap<FrameNumber, DecodedVideoFrame<V>>,
    audio: BTreeMap<FrameNumber, AudioFramePacket<A>>,
    next_decode_frame: Option<FrameNumber>,
}

impl<V, A> Default for PlayoutBuffer<V, A> {
    fn default() -> Self {
        Self {
            video: BTreeMap::new(),
            audio: BTreeMap::new(),
            next_decode_frame: None,
        }
    }
}

impl<V, A> PlayoutBuffer<V, A> {
    fn clear(&mut self) {
        self.video.clear();
        self.audio.clear();
        self.next_decode_frame = None;
    }

    fn reset_for_frame(&mut self, frame: FrameNumber) {
        self.clear();
        self.next_decode_frame = Some(frame);
    }

    fn has_ready_frame(&self, source: &EngineSourceHandle, frame: FrameNumber) -> bool {
        source
            .video_format
            .as_ref()
            .map_or(true, |_| self.video.contains_key(&frame))
            && source
                .audio_format
                .as_ref()
                .map_or(true, |_| self.audio.contains_key(&frame))
    }

    fn newest_ready_frame(&self, source: &EngineSourceHandle) -> Option<FrameNumber> {
        match (source.video_format.is_some(), source.audio_format.is_some()) {
            (true, true) => {
                Some((*self.video.keys().next_back()?).min(*self.audio.keys().next_back()?))
            }
            (true, false) => self.video.keys().next_back().copied(),
            (false, true) => self.audio.keys().next_back().copied(),
            (false, false) => None,
        }
    }

    fn trim_before(&mut self, frame: FrameNumber) {
        self.video.retain(|candidate, _| *candidate >= frame);
        self.audio.retain(|candidate, _| *candidate >= frame);
    }
}

fn refill_budget(playing: bool, ahead: usize, healthy: usize, max_burst: usize) -> usize {
    let healthy = healthy.max(1);
    let max_burst = max_burst.max(1);
    if playing && ahead >= healthy {
        return 0;
    }
    healthy.saturating_sub(ahead).max(1).min(max_burst)
}

fn source_range(source: &EngineSourceHandle) -> Option<FrameRange> {
    FrameRange::new(0, source.duration_frames).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_contract::{AudioFramePacket, DecodedVideoFrame};
    use crate::model::{AudioFormat, ColorSpace, FieldMode, Timebase, VideoFormat};

    #[test]
    fn play_ticks_decode_audio_and_present_same_frame() {
        let mut engine = fake_engine();
        engine.load_source(&source_runtime(), Some(1)).unwrap();
        engine.play(1_000).unwrap();

        let events = engine.tick(1_000).unwrap();

        assert_eq!(engine.state().carrier_frame, 0);
        assert_event_frame(&events, 0);
        assert!(events.iter().any(|event| matches!(
            event,
            BroadcastEvent::AudioLevelChanged {
                track_id,
                peak_dbfs_x100: -900
            } if track_id == "monitor"
        )));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, BroadcastEvent::FramePresented { frame: 0 }))
        );
    }

    #[test]
    fn play_starts_clock_without_synchronous_preroll() {
        let mut engine = fake_engine();
        engine.load_source(&source_runtime(), Some(1)).unwrap();

        let events = engine.play(1_000).unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            BroadcastEvent::TransportStatusChanged {
                status: TransportStatus::Playing
            }
        )));
        assert!(engine.playout.video.is_empty());
        assert!(engine.playout.audio.is_empty());
    }

    #[test]
    fn delayed_tick_catches_up_without_skipping_frames() {
        let mut engine = fake_engine();
        engine.load_source(&source_runtime(), Some(1)).unwrap();
        engine.play(0).unwrap();

        let events = engine.tick(100_000_000).unwrap();

        assert_eq!(engine.state().carrier_frame, 3);
        for expected in 0..=3 {
            assert!(events.iter().any(|event| {
                matches!(event, BroadcastEvent::FramePresented { frame } if *frame == expected)
            }));
        }
        assert!(events.iter().all(|event| {
            !matches!(event, BroadcastEvent::FramePresented { frame } if *frame > 3)
        }));
    }

    #[test]
    fn underrun_recovery_refills_cushion_instead_of_single_frame() {
        let mut engine = fake_engine().with_decode_burst_frames(1);
        engine.load_source(&source_runtime(), Some(1)).unwrap();
        engine.play(0).unwrap();

        engine.tick(0).unwrap();

        assert_eq!(engine.state().carrier_frame, 0);
        assert!(engine.playout.video.contains_key(&1));
        assert!(engine.playout.video.contains_key(&3));
        assert!(engine.playout.audio.contains_key(&1));
        assert!(engine.playout.audio.contains_key(&3));
    }

    #[test]
    fn video_only_source_does_not_call_audio_output() {
        let mut engine = video_only_engine();
        engine
            .load_source(&video_only_source_runtime(), None)
            .unwrap();
        engine.play(0).unwrap();

        let events = engine.tick(0).unwrap();

        assert_event_frame(&events, 0);
        assert!(events.iter().any(|event| {
            matches!(event, BroadcastEvent::FramePresented { frame } if *frame == 0)
        }));
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, BroadcastEvent::AudioLevelChanged { .. }))
        );
    }

    #[test]
    fn audio_only_source_does_not_call_video_decode_or_presenter() {
        let mut engine = audio_only_engine();
        engine
            .load_source(&audio_only_source_runtime(), None)
            .unwrap();
        engine.play(0).unwrap();

        let events = engine.tick(0).unwrap();

        assert_event_frame(&events, 0);
        assert!(events.iter().any(|event| matches!(
            event,
            BroadcastEvent::AudioLevelChanged {
                track_id,
                peak_dbfs_x100: -900
            } if track_id == "monitor"
        )));
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, BroadcastEvent::FramePresented { .. }))
        );
    }

    #[test]
    fn source_without_declared_tracks_is_rejected() {
        let mut engine = fake_engine();
        let source =
            SourceRuntime::new("metadata-only", 20, Timebase::new(50, 1).unwrap()).unwrap();

        let error = engine.load_source(&source, None).unwrap_err();

        assert_eq!(error.kind, BroadcastEngineErrorKind::Contract);
        assert_eq!(error.source_id.as_deref(), Some("metadata-only"));
    }

    #[test]
    fn preload_source_prepares_without_changing_active_source() {
        let mut engine = fake_engine();
        engine.load_source(&source_runtime(), Some(1)).unwrap();
        let next_source = source_runtime_with_id("src-b");

        let events = engine.preload_source(&next_source, Some(2)).unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            BroadcastEvent::SourcePreloaded { source_id } if source_id == "src-b"
        )));
        assert_eq!(engine.state().source.as_ref().unwrap().source_id, "src");
        assert!(engine.state().preloaded_sources.contains_key("src-b"));
        assert_eq!(
            engine.state().preloaded_sources["src-b"].source_revision,
            Some(2)
        );
    }

    #[test]
    fn set_active_source_uses_preloaded_runtime() {
        let mut engine = fake_engine();
        engine.load_source(&source_runtime(), Some(1)).unwrap();
        let next_source = source_runtime_with_id("src-b");
        engine.preload_source(&next_source, Some(2)).unwrap();

        let events = engine.set_active_source(&next_source, Some(3)).unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            BroadcastEvent::SourceReady { source_id } if source_id == "src-b"
        )));
        assert_eq!(engine.state().source.as_ref().unwrap().source_id, "src-b");
        assert_eq!(
            engine.state().source.as_ref().unwrap().source_revision,
            Some(2)
        );
        assert!(engine.state().preloaded_sources.is_empty());
        assert_eq!(engine.state().carrier_frame, 0);
        assert_eq!(engine.state().status, TransportStatus::Ready);
    }

    #[test]
    fn pause_interrupts_clock_until_play_resumes() {
        let mut engine = fake_engine();
        engine.load_source(&source_runtime(), None).unwrap();
        engine.play(0).unwrap();
        engine.pause().unwrap();

        let events = engine.tick(40_000_000).unwrap();

        assert!(events.is_empty());
        assert_eq!(engine.state().status, TransportStatus::Paused);
    }

    #[test]
    fn stop_interrupts_clock_without_rewinding_carrier() {
        let mut engine = fake_engine();
        engine.load_source(&source_runtime(), None).unwrap();
        engine.play(0).unwrap();
        engine.tick(0).unwrap();
        engine.tick(20_000_000).unwrap();

        let events = engine.stop().unwrap();

        assert_eq!(engine.state().carrier_frame, 1);
        assert!(events.iter().any(|event| matches!(
            event,
            BroadcastEvent::TransportStatusChanged {
                status: TransportStatus::Stopped
            }
        )));
        assert!(engine.tick(80_000_000).unwrap().is_empty());
    }

    #[test]
    fn playback_request_sync_cues_requested_start_and_presents_it() {
        let mut engine = fake_engine();
        engine.load_source(&source_runtime(), None).unwrap();

        let events = engine
            .sync_range_runtime(Some(FrameRange::new(10, 20).unwrap()), 10, true)
            .unwrap();

        assert_eq!(engine.state().carrier_frame, 10);
        assert_eq!(engine.state().status, TransportStatus::Paused);
        assert_event_frame(&events, 10);
    }

    #[test]
    fn request_rate_is_applied_when_play_starts() {
        let mut engine = fake_engine();
        engine.load_source(&source_runtime(), None).unwrap();
        engine.apply_request_rate(2, 1).unwrap();
        engine.play(0).unwrap();

        let first = engine.tick(0).unwrap();
        let next = engine.tick(20_000_000).unwrap();

        assert_event_frame(&first, 0);
        assert_event_frame(&next, 1);
        assert_eq!(engine.state().playback_rate_num, 2);
        assert_eq!(engine.state().playback_rate_den, 1);
    }

    #[test]
    fn playback_boundary_pauses_after_presenting_boundary_frame() {
        let mut engine = fake_engine();
        engine.load_source(&source_runtime(), None).unwrap();
        engine
            .sync_range_runtime(Some(FrameRange::new(10, 20).unwrap()), 19, true)
            .unwrap();
        engine.play(0).unwrap();

        let mut events = engine.tick(0).unwrap();
        events.extend(engine.tick(20_000_000).unwrap());
        events.extend(engine.tick(40_000_000).unwrap());

        let frame_presented_index = event_index(&events, |event| {
            matches!(event, BroadcastEvent::FramePresented { frame: 20 })
        });
        let at_out_index = event_index(&events, |event| {
            matches!(event, BroadcastEvent::PlaybackBoundaryReached { frame: 20 })
        });
        let paused_index = event_index(&events, |event| {
            matches!(
                event,
                BroadcastEvent::TransportStatusChanged {
                    status: TransportStatus::Paused
                }
            )
        });
        assert!(frame_presented_index < at_out_index);
        assert!(at_out_index < paused_index);
        assert_eq!(engine.state().carrier_frame, 20);
        assert_eq!(engine.state().status, TransportStatus::Paused);
    }

    #[test]
    fn delayed_tick_still_presents_boundary_frame_before_boundary_event() {
        let mut engine = fake_engine();
        engine.load_source(&source_runtime(), None).unwrap();
        engine
            .sync_range_runtime(Some(FrameRange::new(10, 20).unwrap()), 18, true)
            .unwrap();
        engine.play(0).unwrap();

        let mut events = engine.tick(0).unwrap();
        events.extend(engine.tick(20_000_000).unwrap());
        events.extend(engine.tick(40_000_000).unwrap());

        let frame_presented_index = event_index(&events, |event| {
            matches!(event, BroadcastEvent::FramePresented { frame: 20 })
        });
        let boundary_index = event_index(&events, |event| {
            matches!(event, BroadcastEvent::PlaybackBoundaryReached { frame: 20 })
        });
        assert!(frame_presented_index < boundary_index);
        assert_eq!(engine.state().carrier_frame, 20);
        assert_eq!(engine.state().status, TransportStatus::Paused);
        assert!(events.iter().all(|event| !matches!(
            event,
            BroadcastEvent::FramePresented { frame } if *frame > 20
        )));
    }

    #[test]
    fn load_source_open_failure_keeps_active_engine_state() {
        let mut engine = rejecting_open_engine();
        engine.load_source(&source_runtime(), Some(1)).unwrap();

        let err = engine
            .load_source(&source_runtime_with_id("bad"), Some(2))
            .unwrap_err();

        assert_eq!(err.kind, BroadcastEngineErrorKind::SourceOpen);
        assert_eq!(engine.state().source.as_ref().unwrap().source_id, "src");
        assert_eq!(
            engine.state().source.as_ref().unwrap().source_revision,
            Some(1)
        );
        assert_eq!(engine.state().status, TransportStatus::Ready);
        assert_eq!(
            engine.state().active_range.unwrap(),
            FrameRange::new(0, 20).unwrap()
        );
    }

    #[test]
    fn transport_engine_state_serialization_is_neutral() {
        let mut engine = fake_engine();
        engine.load_source(&source_runtime(), Some(7)).unwrap();

        let text = serde_json::to_string(engine.state())
            .unwrap()
            .to_ascii_lowercase();
        let value = serde_json::to_value(engine.state()).unwrap();
        let fields = value.as_object().expect("state object");

        assert!(text.contains("frame"));
        assert_eq!(fields.len(), 8);
        for field in [
            "source",
            "preloaded_sources",
            "carrier_frame",
            "status",
            "active_range",
            "playback_rate_num",
            "playback_rate_den",
            "decode_burst_frames",
        ] {
            assert!(fields.contains_key(field), "missing field: {field}");
        }
    }

    fn fake_engine()
    -> TransportEngine<FakeSourceOpen, FakeVideoDecode, FakeAudioOutput, FakePresenter> {
        TransportEngine::new(
            FakeSourceOpen,
            FakeVideoDecode,
            FakeAudioOutput,
            FakePresenter,
        )
    }

    fn video_only_engine()
    -> TransportEngine<FakeSourceOpen, FakeVideoDecode, RejectingAudioOutput, FakePresenter> {
        TransportEngine::new(
            FakeSourceOpen,
            FakeVideoDecode,
            RejectingAudioOutput,
            FakePresenter,
        )
    }

    fn audio_only_engine()
    -> TransportEngine<FakeSourceOpen, RejectingVideoDecode, FakeAudioOutput, RejectingPresenter>
    {
        TransportEngine::new(
            FakeSourceOpen,
            RejectingVideoDecode,
            FakeAudioOutput,
            RejectingPresenter,
        )
    }

    fn rejecting_open_engine()
    -> TransportEngine<RejectBadSourceOpen, FakeVideoDecode, FakeAudioOutput, FakePresenter> {
        TransportEngine::new(
            RejectBadSourceOpen,
            FakeVideoDecode,
            FakeAudioOutput,
            FakePresenter,
        )
    }

    fn source_runtime() -> SourceRuntime {
        source_runtime_with_id("src")
    }

    fn source_runtime_with_id(source_id: &str) -> SourceRuntime {
        SourceRuntime::new(source_id, 20, Timebase::new(50, 1).unwrap())
            .unwrap()
            .with_video_format(
                VideoFormat::new(1920, 1080, FieldMode::Progressive, ColorSpace::Rec709).unwrap(),
            )
            .with_audio_format(AudioFormat::new(48_000, 2).unwrap())
    }

    fn video_only_source_runtime() -> SourceRuntime {
        SourceRuntime::new("video-only", 20, Timebase::new(50, 1).unwrap())
            .unwrap()
            .with_video_format(
                VideoFormat::new(1920, 1080, FieldMode::Progressive, ColorSpace::Rec709).unwrap(),
            )
    }

    fn audio_only_source_runtime() -> SourceRuntime {
        SourceRuntime::new("audio-only", 20, Timebase::new(50, 1).unwrap())
            .unwrap()
            .with_audio_format(AudioFormat::new(48_000, 2).unwrap())
    }

    fn assert_event_frame(events: &[BroadcastEvent], expected_frame: FrameNumber) {
        assert!(events.iter().any(|event| matches!(
            event,
            BroadcastEvent::CarrierPositionChanged { frame, .. } if *frame == expected_frame
        )));
    }

    fn event_index(
        events: &[BroadcastEvent],
        predicate: impl Fn(&BroadcastEvent) -> bool,
    ) -> usize {
        events
            .iter()
            .position(predicate)
            .expect("event should exist")
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

    struct RejectBadSourceOpen;

    impl SourceOpenAdapter for RejectBadSourceOpen {
        fn open_source(
            &mut self,
            source: &SourceRuntime,
            source_revision: Option<u64>,
        ) -> Result<EngineSourceHandle, BroadcastEngineError> {
            if source.source_id == "bad" {
                return Err(BroadcastEngineError::new(
                    BroadcastEngineErrorKind::SourceOpen,
                    "rejected",
                )
                .with_source_id(source.source_id.clone()));
            }
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

    struct RejectingVideoDecode;

    impl VideoDecodeAdapter for RejectingVideoDecode {
        type VideoFrame = FrameNumber;

        fn prepare_video(
            &mut self,
            source: &EngineSourceHandle,
        ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
            Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::VideoDecode,
                "video prepare should not be called",
            )
            .with_source_id(source.source_id.clone()))
        }

        fn decode_video_frame(
            &mut self,
            request: EngineFrameRequest,
        ) -> Result<DecodedVideoFrame<Self::VideoFrame>, BroadcastEngineError> {
            Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::VideoDecode,
                "video decode should not be called",
            )
            .with_source_id(request.source_id)
            .with_frame(request.frame))
        }
    }

    struct FakeAudioOutput;

    impl AudioOutputAdapter for FakeAudioOutput {
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
            Ok(vec![BroadcastEvent::AudioLevelChanged {
                track_id: "monitor".to_string(),
                peak_dbfs_x100: -900,
            }])
        }

        fn stop_audio(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
            Ok(Vec::new())
        }
    }

    struct RejectingAudioOutput;

    impl AudioOutputAdapter for RejectingAudioOutput {
        type AudioPacket = FrameNumber;

        fn prepare_audio(
            &mut self,
            source: &EngineSourceHandle,
        ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
            Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::AudioOutput,
                "audio prepare should not be called",
            )
            .with_source_id(source.source_id.clone()))
        }

        fn render_audio_for_frame(
            &mut self,
            request: EngineFrameRequest,
        ) -> Result<AudioFramePacket<Self::AudioPacket>, BroadcastEngineError> {
            Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::AudioOutput,
                "audio render should not be called",
            )
            .with_source_id(request.source_id)
            .with_frame(request.frame))
        }

        fn submit_audio_packet(
            &mut self,
            packet: AudioFramePacket<Self::AudioPacket>,
        ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
            Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::AudioOutput,
                "audio submit should not be called",
            )
            .with_source_id(packet.source_id)
            .with_frame(packet.start_frame))
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

    struct RejectingPresenter;

    impl FramePresenter for RejectingPresenter {
        type VideoFrame = FrameNumber;

        fn present_frame(
            &mut self,
            frame: DecodedVideoFrame<Self::VideoFrame>,
        ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
            Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::VideoPresent,
                "frame presenter should not be called",
            )
            .with_source_id(frame.source_id)
            .with_frame(frame.frame))
        }
    }
}
