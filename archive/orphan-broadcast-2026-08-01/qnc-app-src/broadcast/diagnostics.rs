//! Broadcast playout diagnostics.
//!
//! Diagnostics are read-only. They do not own or advance the clock. They turn
//! queue spans and playout readiness into concrete problem categories that UI
//! or logs can show without guessing why playback stuttered.

use super::playout::{BroadcastPlayoutFrame, PlayoutReadiness};
use super::presentation::PresentationPlanQueue;
use super::timebase::FrameNumber;
use super::video::VideoFrameQueue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueSnapshot {
    pub len: usize,
    pub oldest_frame: Option<FrameNumber>,
    pub newest_frame: Option<FrameNumber>,
}

impl QueueSnapshot {
    pub fn frames_ahead_of(self, master_frame: FrameNumber) -> Option<i64> {
        self.newest_frame.map(|newest| newest.0 - master_frame.0)
    }

    pub fn is_empty(self) -> bool {
        self.len == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayoutProblem {
    NoPresentation,
    PresentationBehind { held_frame: FrameNumber },
    VideoMissing,
    VideoBehind { held_frame: FrameNumber },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BroadcastPlayoutDiagnostics {
    pub master_frame: FrameNumber,
    pub readiness: Option<PlayoutReadiness>,
    pub presentation_queue: QueueSnapshot,
    pub video_queue: QueueSnapshot,
    pub video_required: bool,
    pub problem: Option<PlayoutProblem>,
}

impl BroadcastPlayoutDiagnostics {
    pub fn from_queues<T>(
        master_frame: FrameNumber,
        playout: Option<&BroadcastPlayoutFrame<T>>,
        presentation_queue: &PresentationPlanQueue,
        video_queue: &VideoFrameQueue<T>,
    ) -> Self {
        let readiness = playout.map(BroadcastPlayoutFrame::readiness);
        let video_required = playout
            .map(|frame| frame.presentation.has_video_layers())
            .unwrap_or(false);
        let problem = match readiness {
            None => Some(PlayoutProblem::NoPresentation),
            Some(PlayoutReadiness::Clean | PlayoutReadiness::AudioOnly) => None,
            Some(PlayoutReadiness::PresentationHold { held_frame }) => {
                Some(PlayoutProblem::PresentationBehind { held_frame })
            }
            Some(PlayoutReadiness::VideoHold { held_frame }) => {
                Some(PlayoutProblem::VideoBehind { held_frame })
            }
            Some(PlayoutReadiness::VideoMissing) => Some(PlayoutProblem::VideoMissing),
        };

        Self {
            master_frame,
            readiness,
            presentation_queue: QueueSnapshot {
                len: presentation_queue.len(),
                oldest_frame: presentation_queue.oldest_frame(),
                newest_frame: presentation_queue.newest_frame(),
            },
            video_queue: QueueSnapshot {
                len: video_queue.len(),
                oldest_frame: video_queue.oldest_frame(),
                newest_frame: video_queue.newest_frame(),
            },
            video_required,
            problem,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::timebase::{FrameRange, Timebase};
    use crate::broadcast::{
        AudioMix, BroadcastFrameScheduler, BroadcastPlayoutSelector, BroadcastPresentationBatch,
        BroadcastProgramGraph, BroadcastRenderPlan, CelluloidTrack, PresentationPlanQueue,
        UniversalTimelineSpec, VideoFrameQueue, VideoLayerSourceSpec, VirtualMediaRef,
    };

    fn presentation_batch_with_base_video(
        start: FrameNumber,
        len: usize,
    ) -> BroadcastPresentationBatch {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        let mut spec = UniversalTimelineSpec::new(carrier).with_base_video(
            VideoLayerSourceSpec::VirtualShot(VirtualMediaRef::new("base", "clip_base")),
        );
        spec.add_off_vo_audio(VirtualMediaRef::new("vo", "clip_vo"));
        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let decode_batch = crate::broadcast::FrameDecodeBatch::from_scheduler(
            &render_plan,
            &scheduler,
            start,
            len,
        );

        BroadcastPresentationBatch::try_from_decode_batch(&decode_batch).unwrap()
    }

    fn presentation_batch_audio_only(start: FrameNumber, len: usize) -> BroadcastPresentationBatch {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap_vo",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        let mut spec = UniversalTimelineSpec::new(carrier);
        spec.add_off_vo_audio_with_mix(
            VirtualMediaRef::new("vo", "clip_vo"),
            AudioMix::with_gain_db_tenths(-60),
        );
        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let decode_batch = crate::broadcast::FrameDecodeBatch::from_scheduler(
            &render_plan,
            &scheduler,
            start,
            len,
        );

        BroadcastPresentationBatch::try_from_decode_batch(&decode_batch).unwrap()
    }

    #[test]
    fn diagnostics_report_no_problem_for_clean_playout() {
        let mut presentation_queue = PresentationPlanQueue::new(8);
        let mut video_queue = VideoFrameQueue::new(8);
        presentation_queue.push_batch(presentation_batch_with_base_video(FrameNumber(10), 3));
        video_queue.push_decoded(FrameNumber(10), "f10");
        video_queue.push_decoded(FrameNumber(11), "f11");

        let playout = BroadcastPlayoutSelector::select(
            FrameNumber(11),
            &mut presentation_queue,
            &mut video_queue,
        );
        let diagnostics = BroadcastPlayoutDiagnostics::from_queues(
            FrameNumber(11),
            playout.as_ref(),
            &presentation_queue,
            &video_queue,
        );

        assert_eq!(diagnostics.readiness, Some(PlayoutReadiness::Clean));
        assert_eq!(diagnostics.problem, None);
        assert!(diagnostics.video_required);
        assert_eq!(diagnostics.video_queue.newest_frame, Some(FrameNumber(11)));
    }

    #[test]
    fn diagnostics_report_video_missing() {
        let mut presentation_queue = PresentationPlanQueue::new(8);
        let mut video_queue: VideoFrameQueue<&'static str> = VideoFrameQueue::new(8);
        presentation_queue.push_batch(presentation_batch_with_base_video(FrameNumber(10), 3));

        let playout = BroadcastPlayoutSelector::select(
            FrameNumber(10),
            &mut presentation_queue,
            &mut video_queue,
        );
        let diagnostics = BroadcastPlayoutDiagnostics::from_queues(
            FrameNumber(10),
            playout.as_ref(),
            &presentation_queue,
            &video_queue,
        );

        assert_eq!(diagnostics.readiness, Some(PlayoutReadiness::VideoMissing));
        assert_eq!(diagnostics.problem, Some(PlayoutProblem::VideoMissing));
        assert!(diagnostics.video_required);
        assert!(diagnostics.video_queue.is_empty());
    }

    #[test]
    fn diagnostics_do_not_report_problem_for_expected_audio_only() {
        let mut presentation_queue = PresentationPlanQueue::new(8);
        let mut video_queue: VideoFrameQueue<&'static str> = VideoFrameQueue::new(8);
        presentation_queue.push_batch(presentation_batch_audio_only(FrameNumber(10), 3));

        let playout = BroadcastPlayoutSelector::select(
            FrameNumber(11),
            &mut presentation_queue,
            &mut video_queue,
        );
        let diagnostics = BroadcastPlayoutDiagnostics::from_queues(
            FrameNumber(11),
            playout.as_ref(),
            &presentation_queue,
            &video_queue,
        );

        assert_eq!(diagnostics.readiness, Some(PlayoutReadiness::AudioOnly));
        assert_eq!(diagnostics.problem, None);
        assert!(!diagnostics.video_required);
    }

    #[test]
    fn diagnostics_report_no_presentation_when_queue_has_no_ready_plan() {
        let presentation_queue = PresentationPlanQueue::new(8);
        let video_queue: VideoFrameQueue<&'static str> = VideoFrameQueue::new(8);

        let diagnostics = BroadcastPlayoutDiagnostics::from_queues(
            FrameNumber(10),
            None,
            &presentation_queue,
            &video_queue,
        );

        assert_eq!(diagnostics.readiness, None);
        assert_eq!(diagnostics.problem, Some(PlayoutProblem::NoPresentation));
        assert!(diagnostics.presentation_queue.is_empty());
    }
}
