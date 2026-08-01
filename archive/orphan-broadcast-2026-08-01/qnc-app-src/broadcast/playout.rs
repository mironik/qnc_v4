//! Broadcast playout selection.
//!
//! Playout is the final player-facing selection step. It does not own time, it
//! does not decode, and it does not recompute timeline decisions. Given the
//! current carrier/master frame, it selects the latest ready presentation plan
//! and the latest ready decoded video payload from their queues.

use super::presentation::{BroadcastPresentationPlan, PresentationPlanQueue};
use super::timebase::FrameNumber;
use super::video::{QueuedVideoFrame, VideoFrameQueue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayoutTiming {
    OnTime,
    HoldPrevious { held_frame: FrameNumber },
}

impl PlayoutTiming {
    pub fn for_frame(master_frame: FrameNumber, selected_frame: FrameNumber) -> Self {
        if selected_frame == master_frame {
            Self::OnTime
        } else {
            Self::HoldPrevious {
                held_frame: selected_frame,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlayoutVideo<T> {
    NoVideoExpected,
    Ready(QueuedVideoFrame<T>),
    HoldPrevious(QueuedVideoFrame<T>),
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayoutReadiness {
    Clean,
    AudioOnly,
    PresentationHold { held_frame: FrameNumber },
    VideoHold { held_frame: FrameNumber },
    VideoMissing,
}

impl<T> PlayoutVideo<T> {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::NoVideoExpected | Self::Ready(_))
    }

    pub fn is_hold(&self) -> bool {
        matches!(self, Self::HoldPrevious(_))
    }

    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BroadcastPlayoutFrame<T> {
    pub master_frame: FrameNumber,
    pub presentation_timing: PlayoutTiming,
    pub presentation: BroadcastPresentationPlan,
    pub video: PlayoutVideo<T>,
}

impl<T> BroadcastPlayoutFrame<T> {
    pub fn source_frame(&self) -> FrameNumber {
        self.presentation.source_frame
    }

    pub fn pts_sec(&self) -> f64 {
        self.presentation.pts_sec
    }

    pub fn is_clean_for_playout(&self) -> bool {
        self.presentation_timing == PlayoutTiming::OnTime && self.video.is_ready()
    }

    pub fn readiness(&self) -> PlayoutReadiness {
        if let PlayoutTiming::HoldPrevious { held_frame } = self.presentation_timing {
            return PlayoutReadiness::PresentationHold { held_frame };
        }

        match &self.video {
            PlayoutVideo::NoVideoExpected => PlayoutReadiness::AudioOnly,
            PlayoutVideo::Ready(_) => PlayoutReadiness::Clean,
            PlayoutVideo::HoldPrevious(frame) => PlayoutReadiness::VideoHold {
                held_frame: frame.frame,
            },
            PlayoutVideo::Missing => PlayoutReadiness::VideoMissing,
        }
    }
}

pub struct BroadcastPlayoutSelector;

impl BroadcastPlayoutSelector {
    pub fn select<T: Clone>(
        master_frame: FrameNumber,
        presentation_queue: &mut PresentationPlanQueue,
        video_queue: &mut VideoFrameQueue<T>,
    ) -> Option<BroadcastPlayoutFrame<T>> {
        let presentation = presentation_queue.plan_for_program_clock(master_frame)?;
        let presentation_timing = PlayoutTiming::for_frame(master_frame, presentation.source_frame);
        let video = if !presentation.has_video_layers() {
            PlayoutVideo::NoVideoExpected
        } else {
            match video_queue.frame_for_program_clock(presentation.source_frame) {
                Some(frame) if frame.frame == presentation.source_frame => {
                    PlayoutVideo::Ready(frame)
                }
                Some(frame) => PlayoutVideo::HoldPrevious(frame),
                None => PlayoutVideo::Missing,
            }
        };

        Some(BroadcastPlayoutFrame {
            master_frame,
            presentation_timing,
            presentation,
            video,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::timebase::{FrameRange, Timebase};
    use crate::broadcast::{
        AudioMix, BroadcastFrameScheduler, BroadcastPresentationBatch, BroadcastProgramGraph,
        BroadcastRenderPlan, CelluloidTrack, UniversalTimelineSpec, VideoLayerSourceSpec,
        VirtualMediaRef,
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
    fn playout_returns_audio_only_frame_without_requiring_video_payload() {
        let mut presentation_queue = PresentationPlanQueue::new(8);
        let mut video_queue: VideoFrameQueue<&'static str> = VideoFrameQueue::new(8);
        presentation_queue.push_batch(presentation_batch_audio_only(FrameNumber(10), 3));

        let frame = BroadcastPlayoutSelector::select(
            FrameNumber(11),
            &mut presentation_queue,
            &mut video_queue,
        )
        .unwrap();

        assert_eq!(frame.source_frame(), FrameNumber(11));
        assert_eq!(frame.video, PlayoutVideo::NoVideoExpected);
        assert!(frame.is_clean_for_playout());
        assert_eq!(frame.readiness(), PlayoutReadiness::AudioOnly);
    }

    #[test]
    fn playout_returns_ready_video_when_payload_matches_presentation_frame() {
        let mut presentation_queue = PresentationPlanQueue::new(8);
        let mut video_queue = VideoFrameQueue::new(8);
        presentation_queue.push_batch(presentation_batch_with_base_video(FrameNumber(10), 3));
        video_queue.push_decoded(FrameNumber(10), "f10");
        video_queue.push_decoded(FrameNumber(11), "f11");

        let frame = BroadcastPlayoutSelector::select(
            FrameNumber(11),
            &mut presentation_queue,
            &mut video_queue,
        )
        .unwrap();

        assert_eq!(frame.presentation_timing, PlayoutTiming::OnTime);
        assert_eq!(
            frame.video,
            PlayoutVideo::Ready(QueuedVideoFrame {
                frame: FrameNumber(11),
                payload: "f11"
            })
        );
        assert!(frame.is_clean_for_playout());
        assert_eq!(frame.readiness(), PlayoutReadiness::Clean);
    }

    #[test]
    fn playout_marks_missing_video_when_payload_is_not_ready() {
        let mut presentation_queue = PresentationPlanQueue::new(8);
        let mut video_queue: VideoFrameQueue<&'static str> = VideoFrameQueue::new(8);
        presentation_queue.push_batch(presentation_batch_with_base_video(FrameNumber(10), 3));

        let frame = BroadcastPlayoutSelector::select(
            FrameNumber(10),
            &mut presentation_queue,
            &mut video_queue,
        )
        .unwrap();

        assert!(frame.video.is_missing());
        assert!(!frame.is_clean_for_playout());
        assert_eq!(frame.readiness(), PlayoutReadiness::VideoMissing);
    }

    #[test]
    fn playout_marks_hold_when_only_older_video_payload_is_available() {
        let mut presentation_queue = PresentationPlanQueue::new(8);
        let mut video_queue = VideoFrameQueue::new(8);
        presentation_queue.push_batch(presentation_batch_with_base_video(FrameNumber(10), 3));
        video_queue.push_decoded(FrameNumber(10), "f10");

        let frame = BroadcastPlayoutSelector::select(
            FrameNumber(11),
            &mut presentation_queue,
            &mut video_queue,
        )
        .unwrap();

        assert_eq!(
            frame.video,
            PlayoutVideo::HoldPrevious(QueuedVideoFrame {
                frame: FrameNumber(10),
                payload: "f10"
            })
        );
        assert!(frame.video.is_hold());
        assert!(!frame.is_clean_for_playout());
        assert_eq!(
            frame.readiness(),
            PlayoutReadiness::VideoHold {
                held_frame: FrameNumber(10)
            }
        );
    }

    #[test]
    fn playout_marks_presentation_hold_when_batch_is_behind_master_clock() {
        let mut presentation_queue = PresentationPlanQueue::new(8);
        let mut video_queue = VideoFrameQueue::new(8);
        presentation_queue.push_batch(presentation_batch_with_base_video(FrameNumber(10), 2));
        video_queue.push_decoded(FrameNumber(11), "f11");

        let frame = BroadcastPlayoutSelector::select(
            FrameNumber(20),
            &mut presentation_queue,
            &mut video_queue,
        )
        .unwrap();

        assert_eq!(
            frame.presentation_timing,
            PlayoutTiming::HoldPrevious {
                held_frame: FrameNumber(11)
            }
        );
        assert_eq!(frame.source_frame(), FrameNumber(11));
        assert!(!frame.is_clean_for_playout());
        assert_eq!(
            frame.readiness(),
            PlayoutReadiness::PresentationHold {
                held_frame: FrameNumber(11)
            }
        );
    }
}
