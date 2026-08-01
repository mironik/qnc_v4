//! Broadcast playback session.
//!
//! Transport contract for one program clock + queues. Forms own editorial
//! meaning; this session only runs the layered program it is given.

use std::time::Instant;

use super::audio::{AudioFrameQueue, QueuedAudioFrame};
use super::backend::FrameDecodePlan;
use super::clock::{BroadcastMasterClock, ClockReference, ClockState};
use super::diagnostics::BroadcastPlayoutDiagnostics;
use super::graph::BroadcastProgramGraph;
use super::playout::{BroadcastPlayoutFrame, BroadcastPlayoutSelector};
use super::presentation::{
    BroadcastPresentationBatch, BroadcastPresentationPlan, PresentationPlanError,
    PresentationPlanQueue,
};
use super::render::BroadcastRenderPlan;
use super::schedule::{BroadcastFrameScheduler, ScheduledProgramFrame};
use super::timebase::FrameNumber;
use super::video::{QueuedVideoFrame, VideoFrameQueue};
use super::window::FrameDecodeBatch;
use super::BroadcastPlaybackSource;

#[derive(Debug, Clone)]
pub struct BroadcastPlaybackSession<V, A = V> {
    source: BroadcastPlaybackSource,
    program_graph: BroadcastProgramGraph,
    render_plan: BroadcastRenderPlan,
    scheduler: BroadcastFrameScheduler,
    clock: BroadcastMasterClock,
    video_queue: VideoFrameQueue<V>,
    audio_queue: AudioFrameQueue<A>,
    presentation_queue: PresentationPlanQueue,
}

impl<V, A> BroadcastPlaybackSession<V, A> {
    pub fn new(
        source: BroadcastPlaybackSource,
        queue_capacity: usize,
        reference: ClockReference,
    ) -> Self {
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source);
        Self::from_graph(source, graph, queue_capacity, reference)
    }

    /// Full Kodak program: carrier + layers from [`UniversalTimelineSpec`].
    pub fn from_timeline(
        source: BroadcastPlaybackSource,
        spec: crate::broadcast::UniversalTimelineSpec,
        queue_capacity: usize,
        reference: ClockReference,
    ) -> Self {
        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        Self::from_graph(source, graph, queue_capacity, reference)
    }

    pub fn from_graph(
        source: BroadcastPlaybackSource,
        program_graph: BroadcastProgramGraph,
        queue_capacity: usize,
        reference: ClockReference,
    ) -> Self {
        // Program clock follows the celluloid carrier — not the open-clip identity.
        // Decode/schedule already use render_plan.carrier; mismatch here caused
        // Wrap (and any layered program) to jump/desync.
        let clock = BroadcastMasterClock::new(
            program_graph.carrier.timebase,
            program_graph.carrier.source_range,
            reference,
        );
        let render_plan = BroadcastRenderPlan::from_graph(&program_graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        Self {
            source,
            program_graph,
            render_plan,
            scheduler,
            clock,
            video_queue: VideoFrameQueue::new(queue_capacity),
            audio_queue: AudioFrameQueue::new(queue_capacity),
            presentation_queue: PresentationPlanQueue::new(queue_capacity),
        }
    }

    pub fn source(&self) -> &BroadcastPlaybackSource {
        &self.source
    }

    pub fn carrier_timebase(&self) -> crate::broadcast::Timebase {
        self.program_graph.carrier.timebase
    }

    pub fn carrier_range(&self) -> crate::broadcast::FrameRange {
        self.program_graph.carrier.source_range
    }

    pub fn seconds_at_carrier_frame(&self, frame: FrameNumber) -> f64 {
        self.program_graph.carrier.timebase.seconds_at_frame(frame)
    }

    pub fn program_graph(&self) -> &BroadcastProgramGraph {
        &self.program_graph
    }

    pub fn render_plan(&self) -> &BroadcastRenderPlan {
        &self.render_plan
    }

    pub fn state(&self) -> ClockState {
        self.clock.state()
    }

    pub fn clock_reference(&self) -> ClockReference {
        self.clock.reference()
    }

    pub fn has_audio(&self) -> bool {
        // Media audio presence (silence program buses alone do not own the clock).
        self.source.has_audio
    }

    pub fn has_video(&self) -> bool {
        // OFF/VO + covers: overlays live on the render plan even when the carrier
        // source media itself has no base video track.
        !self.render_plan.video_layers.is_empty() || self.source.has_video
    }

    pub fn play_from_source_frame(&mut self, frame: FrameNumber, now: Instant) {
        self.clock.play_from(frame, now);
    }

    pub fn pause(&mut self, now: Instant) {
        self.clock.pause(now);
    }

    pub fn stall_at(&mut self, frame: FrameNumber) {
        self.clock.stall_at(frame);
    }

    pub fn resume_clock(&mut self, now: Instant) {
        self.clock.resume(now);
    }

    pub fn is_clock_stalled(&self) -> bool {
        self.clock.is_stalled()
    }

    pub fn stop(&mut self) {
        self.clock.stop();
        self.video_queue.clear();
        self.audio_queue.clear();
        self.presentation_queue.clear();
    }

    pub fn seek(&mut self, frame: FrameNumber, now: Instant) {
        self.clock.seek(frame, now);
        self.video_queue.clear();
        self.audio_queue.clear();
        self.presentation_queue.clear();
    }

    pub fn current_source_frame(&self, now: Instant) -> FrameNumber {
        self.clock.current_frame(now)
    }

    pub fn next_frame_deadline(&self, now: Instant) -> Option<std::time::Duration> {
        self.clock.next_frame_deadline(now)
    }

    pub fn schedule_current_frame(&self, now: Instant) -> ScheduledProgramFrame {
        self.scheduler
            .schedule_frame(self.current_source_frame(now))
    }

    pub fn decode_plan_current_frame(&self, now: Instant) -> FrameDecodePlan {
        FrameDecodePlan::from_scheduled(&self.render_plan, self.schedule_current_frame(now))
    }

    pub fn presentation_plan_current_frame(
        &self,
        now: Instant,
    ) -> Result<BroadcastPresentationPlan, PresentationPlanError> {
        BroadcastPresentationPlan::try_from_decode_plan(&self.decode_plan_current_frame(now))
    }

    pub fn presentation_batch_current_frame(
        &self,
        now: Instant,
        lookahead_frames: usize,
    ) -> Result<BroadcastPresentationBatch, PresentationPlanError> {
        BroadcastPresentationBatch::try_from_decode_batch(
            &self.decode_batch_current_frame(now, lookahead_frames),
        )
    }

    pub fn decode_batch_from_frame(
        &self,
        start_frame: FrameNumber,
        lookahead_frames: usize,
    ) -> FrameDecodeBatch {
        FrameDecodeBatch::from_scheduler(
            &self.render_plan,
            &self.scheduler,
            start_frame,
            lookahead_frames,
        )
    }

    pub fn decode_batch_current_frame(
        &self,
        now: Instant,
        lookahead_frames: usize,
    ) -> FrameDecodeBatch {
        self.decode_batch_from_frame(self.current_source_frame(now), lookahead_frames)
    }

    pub fn push_decoded_video_frame(&mut self, frame: FrameNumber, payload: V) {
        if self.has_video() {
            self.video_queue.push_decoded(frame, payload);
        }
    }

    pub fn push_decoded_audio_frame(&mut self, frame: FrameNumber, payload: A) {
        if self.has_audio() {
            self.audio_queue.push_decoded(frame, payload);
        }
    }

    pub fn queued_video_len(&self) -> usize {
        self.video_queue.len()
    }

    pub fn queued_audio_len(&self) -> usize {
        self.audio_queue.len()
    }

    pub fn newest_video_frame(&self) -> Option<FrameNumber> {
        self.video_queue.newest_frame()
    }

    pub fn newest_audio_frame(&self) -> Option<FrameNumber> {
        self.audio_queue.newest_frame()
    }

    pub fn take_audio_frame(&mut self, frame: FrameNumber) -> Option<QueuedAudioFrame<A>> {
        self.audio_queue.take_exact_frame(frame)
    }

    pub fn push_presentation_batch(&mut self, batch: BroadcastPresentationBatch) {
        self.presentation_queue.push_batch(batch);
    }

    pub fn queued_presentation_len(&self) -> usize {
        self.presentation_queue.len()
    }

    pub fn playout_diagnostics(
        &self,
        master_frame: FrameNumber,
        playout: Option<&BroadcastPlayoutFrame<V>>,
    ) -> BroadcastPlayoutDiagnostics {
        BroadcastPlayoutDiagnostics::from_queues(
            master_frame,
            playout,
            &self.presentation_queue,
            &self.video_queue,
        )
    }
}

impl<V: Clone, A> BroadcastPlaybackSession<V, A> {
    pub fn presentable_video_frame(&mut self, now: Instant) -> Option<QueuedVideoFrame<V>> {
        let frame = self.clock.current_frame(now);
        self.video_queue.frame_for_program_clock(frame)
    }

    pub fn presentable_presentation_plan(
        &mut self,
        now: Instant,
    ) -> Option<BroadcastPresentationPlan> {
        let frame = self.clock.current_frame(now);
        self.presentation_queue.plan_for_program_clock(frame)
    }

    pub fn playout_frame(&mut self, now: Instant) -> Option<BroadcastPlayoutFrame<V>> {
        let frame = self.clock.current_frame(now);
        BroadcastPlayoutSelector::select(frame, &mut self.presentation_queue, &mut self.video_queue)
    }
}

impl<V, A: Clone> BroadcastPlaybackSession<V, A> {
    pub fn presentable_audio_frame(&mut self, now: Instant) -> Option<QueuedAudioFrame<A>> {
        let frame = self.clock.current_frame(now);
        self.audio_queue.frame_for_program_clock(frame)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::broadcast::timebase::{FrameRange, Timebase};

    fn source(has_audio: bool, has_video: bool) -> BroadcastPlaybackSource {
        BroadcastPlaybackSource {
            project_id: "project".into(),
            virtual_shot_id: "shot".into(),
            clip_id: "clip".into(),
            source_range: FrameRange::new(FrameNumber(100), FrameNumber(200)),
            source_timebase: Timebase::from_source_fps(25.0),
            has_video,
            has_audio,
            audio_channels: if has_audio { 2 } else { 0 },
        }
    }

    #[test]
    fn session_plays_video_only_virtual_shot_without_audio_clock() {
        let mut session: BroadcastPlaybackSession<&'static str> = BroadcastPlaybackSession::new(
            source(false, true),
            4,
            ClockReference::InternalMonotonic,
        );
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);

        assert!(!session.has_audio());
        assert_eq!(
            session.current_source_frame(t0 + Duration::from_millis(80)),
            FrameNumber(102)
        );
    }

    #[test]
    fn session_uses_decoder_queue_as_consumer_not_clock() {
        let mut session: BroadcastPlaybackSession<&'static str> =
            BroadcastPlaybackSession::new(source(true, true), 4, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);
        session.push_decoded_video_frame(FrameNumber(100), "f100");
        session.push_decoded_video_frame(FrameNumber(101), "f101");

        assert_eq!(
            session.presentable_video_frame(t0),
            Some(QueuedVideoFrame {
                frame: FrameNumber(100),
                payload: "f100"
            })
        );
        assert_eq!(
            session.presentable_video_frame(t0 + Duration::from_millis(40)),
            Some(QueuedVideoFrame {
                frame: FrameNumber(101),
                payload: "f101"
            })
        );
    }

    #[test]
    fn session_uses_audio_queue_as_consumer_not_clock() {
        let mut session: BroadcastPlaybackSession<&'static str> = BroadcastPlaybackSession::new(
            source(true, false),
            4,
            ClockReference::InternalMonotonic,
        );
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);
        session.push_decoded_audio_frame(FrameNumber(100), "a100");
        session.push_decoded_audio_frame(FrameNumber(101), "a101");

        assert_eq!(
            session.presentable_audio_frame(t0 + Duration::from_millis(40)),
            Some(QueuedAudioFrame {
                frame: FrameNumber(101),
                payload: "a101"
            })
        );
    }

    #[test]
    fn session_does_not_queue_media_when_source_track_is_absent() {
        let mut session: BroadcastPlaybackSession<&'static str> = BroadcastPlaybackSession::new(
            source(true, false),
            4,
            ClockReference::InternalMonotonic,
        );

        session.push_decoded_video_frame(FrameNumber(100), "ignored");
        assert_eq!(session.queued_video_len(), 0);

        let mut session: BroadcastPlaybackSession<&'static str> = BroadcastPlaybackSession::new(
            source(false, true),
            4,
            ClockReference::InternalMonotonic,
        );
        session.push_decoded_audio_frame(FrameNumber(100), "ignored");
        assert_eq!(session.queued_audio_len(), 0);
    }

    #[test]
    fn session_decodes_audio_only_source_without_video_request() {
        let mut session: BroadcastPlaybackSession<&'static str> = BroadcastPlaybackSession::new(
            source(true, false),
            4,
            ClockReference::InternalMonotonic,
        );
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);

        let plan = session.decode_plan_current_frame(t0);
        assert_eq!(plan.source_frame, FrameNumber(100));
        assert!(plan.video.is_empty());
        assert_eq!(plan.audio.len(), 2);
    }

    #[test]
    fn session_keeps_celluloid_program_graph() {
        let session: BroadcastPlaybackSession<&'static str> = BroadcastPlaybackSession::new(
            source(false, true),
            4,
            ClockReference::InternalMonotonic,
        );

        assert_eq!(session.program_graph().carrier.virtual_shot_id, "shot");
        assert_eq!(
            session.program_graph().carrier.source_range.start,
            FrameNumber(100)
        );
    }

    #[test]
    fn session_exposes_render_schedule_without_filmstrip_decode() {
        let mut session: BroadcastPlaybackSession<&'static str> =
            BroadcastPlaybackSession::new(source(true, true), 4, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);

        let scheduled = session.schedule_current_frame(t0);
        assert_eq!(scheduled.source_frame, FrameNumber(100));
        assert_eq!(scheduled.video_layers.len(), 1);
        assert!(!session.render_plan().has_filmstrip_decoder_input());
    }

    #[test]
    fn session_builds_decode_plan_for_current_carrier_frame() {
        let mut session: BroadcastPlaybackSession<&'static str> =
            BroadcastPlaybackSession::new(source(true, true), 4, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);

        let plan = session.decode_plan_current_frame(t0 + Duration::from_millis(80));
        assert_eq!(plan.source_frame, FrameNumber(102));
        assert_eq!(plan.video.len(), 1);
        assert_eq!(plan.audio.len(), 2);
        assert!(!plan.has_filmstrip_decode_input());
    }

    #[test]
    fn session_builds_presentation_plan_for_current_carrier_frame() {
        let mut session: BroadcastPlaybackSession<&'static str> =
            BroadcastPlaybackSession::new(source(true, true), 4, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);

        let plan = session
            .presentation_plan_current_frame(t0 + Duration::from_millis(80))
            .unwrap();

        assert_eq!(plan.source_frame, FrameNumber(102));
        assert!(plan.has_video_layers());
        assert!(plan.has_audible_audio());
        assert_eq!(plan.video.layers.len(), 1);
        assert_eq!(plan.audio.inputs.len(), 2);
    }

    #[test]
    fn session_presentation_plan_allows_audio_only_source() {
        let mut session: BroadcastPlaybackSession<&'static str> = BroadcastPlaybackSession::new(
            source(true, false),
            4,
            ClockReference::InternalMonotonic,
        );
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);

        let plan = session.presentation_plan_current_frame(t0).unwrap();

        assert_eq!(plan.source_frame, FrameNumber(100));
        assert!(!plan.has_video_layers());
        assert!(plan.has_audible_audio());
        assert!(plan.video.layers.is_empty());
        assert_eq!(plan.audio.inputs.len(), 2);
    }

    #[test]
    fn session_builds_decode_batch_from_current_frame() {
        let mut session: BroadcastPlaybackSession<&'static str> =
            BroadcastPlaybackSession::new(source(true, true), 4, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);

        let batch = session.decode_batch_current_frame(t0 + Duration::from_millis(80), 4);
        assert_eq!(batch.window.start, FrameNumber(102));
        assert_eq!(batch.plans.len(), 4);
        assert!(!batch.has_filmstrip_decode_input());
    }

    #[test]
    fn session_builds_presentation_batch_from_current_frame() {
        let mut session: BroadcastPlaybackSession<&'static str> =
            BroadcastPlaybackSession::new(source(true, true), 4, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);

        let batch = session
            .presentation_batch_current_frame(t0 + Duration::from_millis(80), 4)
            .unwrap();

        assert_eq!(batch.window.start, FrameNumber(102));
        assert_eq!(batch.len(), 4);
        assert_eq!(batch.current().unwrap().source_frame, FrameNumber(102));
        assert!(batch.plans.iter().all(|plan| plan.has_video_layers()));
    }

    #[test]
    fn session_uses_presentation_queue_as_consumer_not_clock() {
        let mut session: BroadcastPlaybackSession<&'static str> =
            BroadcastPlaybackSession::new(source(true, true), 4, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);
        let batch = session.presentation_batch_current_frame(t0, 3).unwrap();
        session.push_presentation_batch(batch);

        assert_eq!(session.queued_presentation_len(), 3);
        assert_eq!(
            session
                .presentable_presentation_plan(t0 + Duration::from_millis(40))
                .map(|plan| plan.source_frame),
            Some(FrameNumber(101))
        );
        assert_eq!(
            session
                .presentable_presentation_plan(t0 + Duration::from_millis(400))
                .map(|plan| plan.source_frame),
            Some(FrameNumber(102))
        );
    }

    #[test]
    fn session_clears_presentation_queue_on_seek_and_stop() {
        let mut session: BroadcastPlaybackSession<&'static str> =
            BroadcastPlaybackSession::new(source(true, true), 4, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);
        let batch = session.presentation_batch_current_frame(t0, 3).unwrap();
        session.push_presentation_batch(batch);
        assert_eq!(session.queued_presentation_len(), 3);

        session.seek(FrameNumber(120), t0);
        assert_eq!(session.queued_presentation_len(), 0);

        let batch = session.presentation_batch_current_frame(t0, 3).unwrap();
        session.push_presentation_batch(batch);
        assert_eq!(session.queued_presentation_len(), 3);
        session.stop();
        assert_eq!(session.queued_presentation_len(), 0);
    }

    #[test]
    fn session_playout_frame_combines_presentation_and_decoded_video() {
        let mut session: BroadcastPlaybackSession<&'static str> =
            BroadcastPlaybackSession::new(source(true, true), 4, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);
        let batch = session.presentation_batch_current_frame(t0, 3).unwrap();
        session.push_presentation_batch(batch);
        session.push_decoded_video_frame(FrameNumber(100), "f100");
        session.push_decoded_video_frame(FrameNumber(101), "f101");

        let frame = session
            .playout_frame(t0 + Duration::from_millis(40))
            .unwrap();

        assert_eq!(frame.source_frame(), FrameNumber(101));
        assert!(frame.is_clean_for_playout());
    }

    #[test]
    fn session_stall_freezes_video_and_audio_presentables_together() {
        let mut session: BroadcastPlaybackSession<&'static str, &'static str> =
            BroadcastPlaybackSession::new(source(true, true), 8, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);
        for f in 100..=108 {
            session.push_decoded_video_frame(FrameNumber(f), "v");
            session.push_decoded_audio_frame(FrameNumber(f), "a");
        }

        // Wall would be far ahead @25fps after 400ms, but stall pins both consumers.
        session.stall_at(FrameNumber(102));
        let later = t0 + Duration::from_millis(400);
        assert!(session.is_clock_stalled());
        assert_eq!(session.current_source_frame(later), FrameNumber(102));

        let video = session.presentable_video_frame(later).unwrap();
        let audio = session.presentable_audio_frame(later).unwrap();
        assert_eq!(video.frame, FrameNumber(102));
        assert_eq!(audio.frame, FrameNumber(102));
    }

    #[test]
    fn session_seek_clears_av_queues() {
        let mut session: BroadcastPlaybackSession<&'static str, &'static str> =
            BroadcastPlaybackSession::new(source(true, true), 8, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        session.push_decoded_video_frame(FrameNumber(100), "v");
        session.push_decoded_audio_frame(FrameNumber(100), "a");
        assert_eq!(session.queued_video_len(), 1);
        assert_eq!(session.queued_audio_len(), 1);

        session.seek(FrameNumber(150), t0);
        assert_eq!(session.queued_video_len(), 0);
        assert_eq!(session.queued_audio_len(), 0);
        assert_eq!(session.current_source_frame(t0), FrameNumber(150));
    }

    #[test]
    fn session_audio_hold_lags_when_master_jumps_without_stall() {
        // Documents hold-policy risk if engine lets master jump: audio presentable
        // snaps to latest ≤ master and intermediates are discarded.
        let mut session: BroadcastPlaybackSession<&'static str, i64> =
            BroadcastPlaybackSession::new(source(true, true), 8, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        session.pause(t0);
        session.seek(FrameNumber(100), t0);
        for f in 100..=104 {
            session.push_decoded_audio_frame(FrameNumber(f), f);
        }
        // Manually advance clock without stall (pause+seek keeps paused).
        session.play_from_source_frame(FrameNumber(100), t0);
        let jumped = session.presentable_audio_frame(t0 + Duration::from_millis(160));
        // 160ms @25fps = 4 frames → master 104; hold returns 104, queue drops 101..103.
        assert_eq!(jumped.map(|q| q.payload), Some(104));
        assert_eq!(session.queued_audio_len(), 1);
    }

    #[test]
    fn session_without_media_audio_ignores_pushed_audio_frames() {
        let mut session: BroadcastPlaybackSession<&'static str, i64> =
            BroadcastPlaybackSession::new(
                source(false, true),
                4,
                ClockReference::InternalMonotonic,
            );
        assert!(!session.has_audio());
        session.push_decoded_audio_frame(FrameNumber(100), 100);
        assert_eq!(session.queued_audio_len(), 0);
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);
        assert_eq!(session.presentable_audio_frame(t0), None);
    }

    #[test]
    fn session_audio_presentable_advances_with_clock_one_frame_at_a_time() {
        let mut session: BroadcastPlaybackSession<&'static str, i64> =
            BroadcastPlaybackSession::new(
                source(true, true),
                16,
                ClockReference::InternalMonotonic,
            );
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);
        for f in 100..=105 {
            session.push_decoded_audio_frame(FrameNumber(f), f);
        }
        let mut heard = Vec::new();
        for ms in [0_u64, 40, 80, 120, 160, 200] {
            if let Some(a) = session.presentable_audio_frame(t0 + Duration::from_millis(ms)) {
                if heard.last() != Some(&a.payload) {
                    heard.push(a.payload);
                }
            }
        }
        assert_eq!(heard, vec![100, 101, 102, 103, 104, 105]);
    }
}
