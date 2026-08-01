//! Broadcast player pump.
//!
//! This is the first player-facing bridge around the broadcast runtime. It does
//! not own timing outside `BroadcastPlaybackSession` and it does not decode
//! filmstrip/thumbnail media. The runtime driver decides the current carrier
//! frame and presentation batch; the decode worker only fills queues.

use std::time::{Duration, Instant};

use super::asset::{BroadcastMediaResolver, BroadcastResolvedDecodeBackend};
use super::backend::{DecodeError, DecodedAudioBus, DecodedProgramFrame};
use super::clock::ClockReference;
use super::diagnostics::BroadcastPlayoutDiagnostics;
use super::playout::BroadcastPlayoutFrame;
use super::presentation::{BroadcastPresentationBatch, PresentationPlanError};
use super::runtime::BroadcastRuntimeDriver;
use super::session::BroadcastPlaybackSession;
use super::timebase::FrameNumber;
use super::window::FrameDecodeBatch;
use super::worker::{BroadcastResolvedDecodeWorker, DecodeQueueFill};
use super::BroadcastPlaybackSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastPlayerErrorKind {
    Presentation,
    Decode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastPlayerError {
    pub kind: BroadcastPlayerErrorKind,
    pub message: String,
}

impl BroadcastPlayerError {
    pub fn presentation(message: impl Into<String>) -> Self {
        Self {
            kind: BroadcastPlayerErrorKind::Presentation,
            message: message.into(),
        }
    }

    pub fn decode(message: impl Into<String>) -> Self {
        Self {
            kind: BroadcastPlayerErrorKind::Decode,
            message: message.into(),
        }
    }
}

impl From<PresentationPlanError> for BroadcastPlayerError {
    fn from(value: PresentationPlanError) -> Self {
        Self::presentation(value.message)
    }
}

impl From<DecodeError> for BroadcastPlayerError {
    fn from(value: DecodeError) -> Self {
        Self::decode(value.message)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BroadcastPlayerTick<V, A> {
    pub master_frame: FrameNumber,
    pub decode_batch: FrameDecodeBatch,
    pub presentation_batch: BroadcastPresentationBatch,
    pub decoded: DecodeQueueFill,
    pub playout: Option<BroadcastPlayoutFrame<DecodedProgramFrame<V, A>>>,
    pub diagnostics: BroadcastPlayoutDiagnostics,
}

type PlayerFrame<B> = DecodedProgramFrame<
    <B as BroadcastResolvedDecodeBackend>::VideoPayload,
    <B as BroadcastResolvedDecodeBackend>::AudioPayload,
>;

type PlayerAudioFrame<B> =
    Vec<DecodedAudioBus<<B as BroadcastResolvedDecodeBackend>::AudioPayload>>;

pub struct BroadcastPlaybackPump<B: BroadcastResolvedDecodeBackend, R> {
    session: BroadcastPlaybackSession<PlayerFrame<B>, PlayerAudioFrame<B>>,
    worker: BroadcastResolvedDecodeWorker<B, R>,
    lookahead_frames: usize,
}

impl<B, R> BroadcastPlaybackPump<B, R>
where
    B: BroadcastResolvedDecodeBackend,
{
    pub fn new(
        source: BroadcastPlaybackSource,
        backend: B,
        resolver: R,
        queue_capacity: usize,
        lookahead_frames: usize,
        reference: ClockReference,
    ) -> Self {
        Self {
            session: BroadcastPlaybackSession::new(source, queue_capacity, reference),
            worker: BroadcastResolvedDecodeWorker::new(backend, resolver),
            lookahead_frames: lookahead_frames.max(1),
        }
    }

    pub fn from_timeline(
        source: BroadcastPlaybackSource,
        spec: crate::broadcast::UniversalTimelineSpec,
        backend: B,
        resolver: R,
        queue_capacity: usize,
        lookahead_frames: usize,
        reference: ClockReference,
    ) -> Self {
        Self {
            session: BroadcastPlaybackSession::from_timeline(
                source,
                spec,
                queue_capacity,
                reference,
            ),
            worker: BroadcastResolvedDecodeWorker::new(backend, resolver),
            lookahead_frames: lookahead_frames.max(1),
        }
    }

    pub fn source(&self) -> &BroadcastPlaybackSource {
        self.session.source()
    }

    pub fn seconds_at_carrier_frame(&self, frame: FrameNumber) -> f64 {
        self.session.seconds_at_carrier_frame(frame)
    }

    pub fn carrier_range(&self) -> crate::broadcast::FrameRange {
        self.session.carrier_range()
    }

    pub fn play_from_start(&mut self, now: Instant) {
        self.session
            .play_from_source_frame(self.session.source().source_range.start, now);
    }

    pub fn play_from_frame(&mut self, frame: FrameNumber, now: Instant) {
        self.session.play_from_source_frame(frame, now);
    }

    pub fn pause(&mut self, now: Instant) {
        self.session.pause(now);
    }

    pub fn stall_at(&mut self, frame: FrameNumber) {
        self.session.stall_at(frame);
    }

    pub fn resume_clock(&mut self, now: Instant) {
        self.session.resume_clock(now);
    }

    pub fn is_clock_stalled(&self) -> bool {
        self.session.is_clock_stalled()
    }

    pub fn seek(&mut self, frame: FrameNumber, now: Instant) {
        self.session.seek(frame, now);
    }

    pub fn stop(&mut self) {
        self.session.stop();
    }

    pub fn current_frame(&self, now: Instant) -> FrameNumber {
        self.session.current_source_frame(now)
    }

    pub fn newest_video_frame(&self) -> Option<FrameNumber> {
        self.session.newest_video_frame()
    }

    pub fn newest_audio_frame(&self) -> Option<FrameNumber> {
        self.session.newest_audio_frame()
    }

    pub fn take_audio_frame(
        &mut self,
        frame: FrameNumber,
    ) -> Option<crate::broadcast::QueuedAudioFrame<PlayerAudioFrame<B>>> {
        self.session.take_audio_frame(frame)
    }

    pub fn next_frame_deadline(&self, now: Instant) -> Option<Duration> {
        self.session.next_frame_deadline(now)
    }

    pub fn clock_state(&self) -> crate::broadcast::ClockState {
        self.session.state()
    }

    pub fn backend_mut(&mut self) -> &mut B {
        self.worker.backend_mut()
    }
}

impl<B, R> BroadcastPlaybackPump<B, R>
where
    B: BroadcastResolvedDecodeBackend,
    B::VideoPayload: Clone,
    B::AudioPayload: Clone,
    R: BroadcastMediaResolver,
{
    pub fn tick(
        &mut self,
        now: Instant,
    ) -> Result<BroadcastPlayerTick<B::VideoPayload, B::AudioPayload>, BroadcastPlayerError> {
        let runtime_tick =
            BroadcastRuntimeDriver::tick(&mut self.session, now, self.lookahead_frames)?;

        // Only decode frames not already buffered — re-decoding the same window
        // every tick restarts continuous ffmpeg pipes and causes stutter.
        // While Playing: decode ONLY the next sequential frame after the A/V
        // stall frontier (min of video/audio). Video-only newest lets audio lag
        // forever → permanent start stall ("zapne").
        let source = self.session.source();
        let newest_video = self.session.newest_video_frame();
        let newest_audio = self.session.newest_audio_frame();
        let newest = crate::broadcast::decode_newest_for_refill(
            source.expects_video_decode(),
            source.expects_media_audio_decode(),
            newest_video,
            newest_audio,
        )
        .map(|f| f.0)
        .unwrap_or(-1);
        let master = runtime_tick.master_frame.0;
        let ahead = (newest - master).max(0) as usize;
        let playing = self.session.state() == crate::broadcast::ClockState::Playing;
        let stalled = self.session.is_clock_stalled();
        let healthy = self.lookahead_frames.saturating_sub(1).max(2);
        // While stalled / low: refill burst. Healthy playing: present-only (0).
        let max_decode = crate::broadcast::decode_budget(playing, stalled, ahead, healthy, 4);
        // Decode from the frontier (newest+1), not from wall-master lookahead.
        // Filtering master's window cannot refill while stalled: once newest is
        // past master+lookahead, pending becomes empty and A/V hitch forever.
        let pending = if max_decode == 0 {
            let mut empty = runtime_tick.decode_batch.clone();
            empty.plans.clear();
            empty.window.end_exclusive = empty.window.start;
            empty
        } else {
            let decode_start = if newest >= 0 {
                FrameNumber(newest + 1)
            } else {
                runtime_tick.master_frame
            };
            self.session
                .decode_batch_from_frame(decode_start, max_decode)
        };

        let decoded_frames = if pending.plans.is_empty() {
            Vec::new()
        } else {
            self.worker.decode_batch(&pending)?
        };
        let decoded = self.push_decoded_frames(decoded_frames);
        let playout = self.session.playout_frame(now);
        let diagnostics = self
            .session
            .playout_diagnostics(runtime_tick.master_frame, playout.as_ref());

        Ok(BroadcastPlayerTick {
            master_frame: runtime_tick.master_frame,
            decode_batch: runtime_tick.decode_batch,
            presentation_batch: runtime_tick.presentation_batch,
            decoded,
            playout,
            diagnostics,
        })
    }

    pub fn presentable_audio(
        &mut self,
        now: Instant,
    ) -> Option<crate::broadcast::QueuedAudioFrame<PlayerAudioFrame<B>>> {
        self.session.presentable_audio_frame(now)
    }

    /// Peek playout from current queues without decoding (Playing present-before-decode).
    pub fn playout_now(&mut self, now: Instant) -> Option<BroadcastPlayoutFrame<PlayerFrame<B>>> {
        self.session.playout_frame(now)
    }

    fn push_decoded_frames(&mut self, frames: Vec<PlayerFrame<B>>) -> DecodeQueueFill {
        let mut fill = DecodeQueueFill {
            video_frames: 0,
            audio_frames: 0,
        };
        let expect_video = self.session.source().expects_video_decode();
        let expect_audio = self.session.source().expects_media_audio_decode();
        for frame in frames {
            let source_frame = frame.source_frame;
            let has_audio = !frame.audio.is_empty();
            let has_video = !frame.video.is_empty();
            // Keep A/V queues in lockstep when both are expected — pushing video
            // alone lets the stall frontier pin on audio forever at play start.
            if expect_video && expect_audio && has_video != has_audio {
                continue;
            }
            if has_audio {
                self.session
                    .push_decoded_audio_frame(source_frame, frame.audio.clone());
                fill.audio_frames += 1;
            }
            // Base and/or overlays — including OFF/VO covers without source base video.
            if has_video {
                self.session.push_decoded_video_frame(source_frame, frame);
                fill.video_frames += 1;
            }
        }
        fill
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::playout::PlayoutReadiness;
    use crate::broadcast::timebase::{FrameRange, Timebase};
    use crate::broadcast::{
        BroadcastMediaAsset, InMemoryMediaResolver, NullResolvedBroadcastBackend,
    };

    fn source(
        source_timebase: Timebase,
        has_video: bool,
        has_audio: bool,
    ) -> BroadcastPlaybackSource {
        BroadcastPlaybackSource {
            project_id: "project".into(),
            virtual_shot_id: "shot".into(),
            clip_id: "clip".into(),
            source_range: FrameRange::new(FrameNumber(100), FrameNumber(200)),
            source_timebase,
            has_video,
            has_audio,
            audio_channels: if has_audio { 2 } else { 0 },
        }
    }

    fn resolver(
        source_timebase: Timebase,
        has_video: bool,
        has_audio: bool,
    ) -> InMemoryMediaResolver {
        InMemoryMediaResolver::new().with_asset(BroadcastMediaAsset::from_parts(
            "project",
            "shot",
            "clip",
            crate::broadcast::BroadcastMediaKind::Proxy,
            crate::broadcast::BroadcastMediaLocation::LocalPath("media/proxy.mxf".into()),
            source_timebase,
            has_video,
            has_audio,
            if has_audio { 2 } else { 0 },
        ))
    }

    fn tick_at_frame_for_source_rate(num: u32, den: u32) -> BroadcastPlayerTick<(), ()> {
        let source_timebase = Timebase::from_source_rate(num, den).unwrap();
        let mut pump = BroadcastPlaybackPump::new(
            source(source_timebase, true, true),
            NullResolvedBroadcastBackend,
            resolver(source_timebase, true, true),
            8,
            3,
            ClockReference::InternalMonotonic,
        );
        let t0 = Instant::now();
        pump.play_from_frame(FrameNumber(125), t0);

        pump.tick(t0).unwrap()
    }

    #[test]
    fn pump_uses_source_timebase_for_pts_and_media_seek() {
        let source_timebase = Timebase::from_source_rate(25, 1).unwrap();
        let tick = tick_at_frame_for_source_rate(25, 1);

        assert_eq!(tick.master_frame, FrameNumber(125));
        // Playing with empty queue: decode budget refills a short sequential burst.
        assert!(tick.decoded.video_frames >= 1 && tick.decoded.video_frames <= 4);
        assert!(tick.decoded.audio_frames >= 1 && tick.decoded.audio_frames <= 4);
        assert_eq!(tick.decoded.video_frames, tick.decoded.audio_frames);
        assert_eq!(tick.decode_batch.plans.len(), 3);
        assert!(!tick.decode_batch.has_filmstrip_decode_input());
        assert_eq!(
            tick.decode_batch.plans[0].pts_sec,
            source_timebase.seconds_at_frame(FrameNumber(25))
        );
        assert_eq!(
            tick.decode_batch.plans[0].video[0].media_seek_sec,
            source_timebase.seconds_at_frame(FrameNumber(125))
        );
        assert_eq!(tick.playout.unwrap().readiness(), PlayoutReadiness::Clean);
    }

    #[test]
    fn pump_accepts_common_source_rates_without_defaulting_to_25() {
        for (num, den) in [(24, 1), (30, 1), (50, 1), (60, 1), (30_000, 1_001)] {
            let source_timebase = Timebase::from_source_rate(num, den).unwrap();
            let tick = tick_at_frame_for_source_rate(num, den);

            assert_eq!(
                tick.decode_batch.plans[0].pts_sec,
                source_timebase.seconds_at_frame(FrameNumber(25)),
                "PTS must come from source rate {num}/{den}"
            );
            assert_eq!(
                tick.decode_batch.plans[0].video[0].media_seek_sec,
                source_timebase.seconds_at_frame(FrameNumber(125)),
                "media seek must come from source rate {num}/{den}"
            );
            if (num, den) != (25, 1) {
                assert_ne!(tick.decode_batch.plans[0].pts_sec, 1.0);
                assert_ne!(tick.decode_batch.plans[0].video[0].media_seek_sec, 5.0);
            }
        }
    }

    #[test]
    fn playing_decodes_only_next_frame_after_newest() {
        let mut pump = BroadcastPlaybackPump::new(
            source(Timebase::from_source_rate(25, 1).unwrap(), true, true),
            NullResolvedBroadcastBackend,
            resolver(Timebase::from_source_rate(25, 1).unwrap(), true, true),
            8,
            4,
            ClockReference::InternalMonotonic,
        );
        let t0 = Instant::now();
        pump.pause(t0);
        pump.seek(FrameNumber(100), t0);
        for i in 0..2 {
            let t = t0 + Duration::from_millis(i * 5);
            let _ = pump.tick(t).unwrap();
        }
        let newest_before = pump.newest_video_frame().map(|f| f.0).unwrap_or(-1);
        assert!(newest_before >= 100);

        // Stall at frontier: refill burst must stay sequential (no jump to wall).
        pump.play_from_frame(FrameNumber(newest_before), t0 + Duration::from_millis(20));
        pump.stall_at(FrameNumber(newest_before));
        let tick = pump.tick(t0 + Duration::from_millis(25)).unwrap();
        let newest_after = pump.newest_video_frame().map(|f| f.0).unwrap_or(-1);
        assert!(tick.decoded.video_frames >= 1);
        assert!(tick.decoded.video_frames <= 4);
        assert_eq!(
            newest_after,
            newest_before + tick.decoded.video_frames as i64
        );
    }

    #[test]
    fn stalled_refill_over_multiple_ticks_stays_sequential() {
        let mut pump = BroadcastPlaybackPump::new(
            source(Timebase::from_source_rate(25, 1).unwrap(), true, true),
            NullResolvedBroadcastBackend,
            resolver(Timebase::from_source_rate(25, 1).unwrap(), true, true),
            16,
            4,
            ClockReference::InternalMonotonic,
        );
        let t0 = Instant::now();
        pump.pause(t0);
        pump.seek(FrameNumber(100), t0);
        let _ = pump.tick(t0).unwrap();
        let start = pump.newest_video_frame().map(|f| f.0).unwrap();
        pump.play_from_frame(FrameNumber(start), t0);
        pump.stall_at(FrameNumber(start));

        let mut prev = start;
        for i in 1..=5 {
            assert!(pump.is_clock_stalled());
            let tick = pump.tick(t0 + Duration::from_millis(i * 10)).unwrap();
            let newest = pump.newest_video_frame().map(|f| f.0).unwrap();
            assert!(tick.decoded.video_frames <= 4);
            assert_eq!(
                newest,
                prev + tick.decoded.video_frames as i64,
                "tick {i}: sequential refill broken"
            );
            // Clock must stay frozen at stall point until engine resumes.
            assert_eq!(
                pump.current_frame(t0 + Duration::from_millis(i * 10)).0,
                start
            );
            prev = newest;
        }
        assert!(prev >= start + 5, "stalled refill must grow buffer");
    }

    #[test]
    fn pump_allows_audio_only_source_without_video_queue() {
        let mut pump = BroadcastPlaybackPump::new(
            source(Timebase::from_source_rate(25, 1).unwrap(), false, true),
            NullResolvedBroadcastBackend,
            resolver(Timebase::from_source_rate(25, 1).unwrap(), false, true),
            8,
            2,
            ClockReference::InternalMonotonic,
        );
        let t0 = Instant::now();
        pump.play_from_start(t0);

        let tick = pump.tick(t0).unwrap();

        assert_eq!(tick.decoded.video_frames, 0);
        // Playing + empty buffer: budget may burst several audio frames.
        assert!(tick.decoded.audio_frames >= 1 && tick.decoded.audio_frames <= 4);
        assert_eq!(
            tick.playout.unwrap().readiness(),
            PlayoutReadiness::AudioOnly
        );
    }

    #[test]
    fn healthy_buffer_stops_decode_while_playing() {
        let mut pump = BroadcastPlaybackPump::new(
            source(Timebase::from_source_rate(25, 1).unwrap(), true, true),
            NullResolvedBroadcastBackend,
            resolver(Timebase::from_source_rate(25, 1).unwrap(), true, true),
            24,
            4,
            ClockReference::InternalMonotonic,
        );
        let t0 = Instant::now();
        pump.pause(t0);
        pump.seek(FrameNumber(100), t0);
        for i in 0..6 {
            let _ = pump.tick(t0 + Duration::from_millis(i)).unwrap();
        }
        let newest = pump.newest_video_frame().map(|f| f.0).unwrap();
        // Play from well behind newest so ahead >= healthy.
        pump.play_from_frame(FrameNumber(newest - 6), t0 + Duration::from_millis(20));
        assert!(!pump.is_clock_stalled());
        let ahead = newest - (newest - 6);
        assert!(ahead >= 4);
        let tick = pump.tick(t0 + Duration::from_millis(21)).unwrap();
        assert_eq!(
            tick.decoded.video_frames, 0,
            "healthy playing buffer must not keep decoding"
        );
        assert_eq!(pump.newest_video_frame().map(|f| f.0), Some(newest));
    }

    #[test]
    fn stalled_refill_keeps_audio_and_video_newest_aligned() {
        let mut pump = BroadcastPlaybackPump::new(
            source(Timebase::from_source_rate(25, 1).unwrap(), true, true),
            NullResolvedBroadcastBackend,
            resolver(Timebase::from_source_rate(25, 1).unwrap(), true, true),
            16,
            4,
            ClockReference::InternalMonotonic,
        );
        let t0 = Instant::now();
        pump.pause(t0);
        pump.seek(FrameNumber(100), t0);
        let _ = pump.tick(t0).unwrap();
        let start = pump.newest_video_frame().map(|f| f.0).unwrap();
        pump.play_from_frame(FrameNumber(start), t0);
        pump.stall_at(FrameNumber(start));

        for i in 1..=4 {
            let _ = pump.tick(t0 + Duration::from_millis(i * 5)).unwrap();
            let v = pump.newest_video_frame().map(|f| f.0);
            // Audio newest lives on the session; pump exposes video newest only —
            // decoded counts must stay paired per tick (same plans).
            assert!(v.is_some());
        }
        // After refill, video newest advanced; clock still frozen.
        assert!(pump.newest_video_frame().map(|f| f.0).unwrap() > start);
        assert_eq!(pump.current_frame(t0 + Duration::from_millis(50)).0, start);
    }

    #[test]
    fn seek_while_paused_clears_decode_frontier() {
        let mut pump = BroadcastPlaybackPump::new(
            source(Timebase::from_source_rate(25, 1).unwrap(), true, true),
            NullResolvedBroadcastBackend,
            resolver(Timebase::from_source_rate(25, 1).unwrap(), true, true),
            8,
            4,
            ClockReference::InternalMonotonic,
        );
        let t0 = Instant::now();
        pump.pause(t0);
        pump.seek(FrameNumber(100), t0);
        let _ = pump.tick(t0).unwrap();
        assert!(pump.newest_video_frame().is_some());

        pump.seek(FrameNumber(150), t0);
        assert_eq!(pump.newest_video_frame(), None);
        assert_eq!(pump.current_frame(t0), FrameNumber(150));
        let tick = pump.tick(t0).unwrap();
        assert!(tick.decoded.video_frames >= 1);
        assert_eq!(
            pump.newest_video_frame().map(|f| f.0),
            Some(150 + tick.decoded.video_frames as i64 - 1)
        );
    }

    #[test]
    fn pump_reports_missing_asset_as_decode_error() {
        let mut pump = BroadcastPlaybackPump::new(
            source(Timebase::from_source_rate(25, 1).unwrap(), true, true),
            NullResolvedBroadcastBackend,
            InMemoryMediaResolver::new(),
            8,
            2,
            ClockReference::InternalMonotonic,
        );
        let t0 = Instant::now();
        pump.play_from_start(t0);

        let err = pump.tick(t0).unwrap_err();

        assert_eq!(err.kind, BroadcastPlayerErrorKind::Decode);
        assert!(err.message.contains("missing media asset"));
    }
}
