//! Broadcast presentation plan for one carrier frame.
//!
//! This is the frame-level boundary a player/presenter can consume: one
//! carrier frame, one PTS, one video composite plan, one audio mix plan, plus
//! marker/effect events. UI code should not recombine decode, audio, and video
//! decisions independently.

use std::collections::VecDeque;

use super::backend::{DecodeEffectEvent, DecodeMarkerEvent, FrameDecodePlan};
use super::compositor::{VideoCompositePlan, VideoCompositePlanError};
use super::mixer::{AudioMixPlan, AudioMixPlanError};
use super::timebase::FrameNumber;
use super::window::{DecodeWindow, FrameDecodeBatch};

#[derive(Debug, Clone, PartialEq)]
pub struct BroadcastPresentationPlan {
    pub source_frame: FrameNumber,
    pub pts_sec: f64,
    pub audio: AudioMixPlan,
    pub video: VideoCompositePlan,
    pub markers: Vec<DecodeMarkerEvent>,
    pub effects: Vec<DecodeEffectEvent>,
}

impl BroadcastPresentationPlan {
    pub fn try_from_decode_plan(plan: &FrameDecodePlan) -> Result<Self, PresentationPlanError> {
        if plan.has_filmstrip_decode_input() {
            return Err(PresentationPlanError::video(
                "filmstrip is not a presentation video input",
            ));
        }

        let audio =
            AudioMixPlan::try_from_decode_plan(plan).map_err(PresentationPlanError::from)?;
        let video =
            VideoCompositePlan::try_from_decode_plan(plan).map_err(PresentationPlanError::from)?;

        Ok(Self {
            source_frame: plan.source_frame,
            pts_sec: plan.pts_sec,
            audio,
            video,
            markers: plan.markers.clone(),
            effects: plan.effects.clone(),
        })
    }

    pub fn has_video_layers(&self) -> bool {
        !self.video.layers.is_empty()
    }

    pub fn has_audible_audio(&self) -> bool {
        self.audio.audible_inputs().next().is_some()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BroadcastPresentationBatch {
    pub window: DecodeWindow,
    pub plans: Vec<BroadcastPresentationPlan>,
}

impl BroadcastPresentationBatch {
    pub fn try_from_decode_batch(batch: &FrameDecodeBatch) -> Result<Self, PresentationPlanError> {
        let mut plans = Vec::with_capacity(batch.plans.len());
        for plan in &batch.plans {
            plans.push(BroadcastPresentationPlan::try_from_decode_plan(plan)?);
        }

        Ok(Self {
            window: batch.window,
            plans,
        })
    }

    pub fn len(&self) -> usize {
        self.plans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plans.is_empty()
    }

    pub fn current(&self) -> Option<&BroadcastPresentationPlan> {
        self.plans.first()
    }

    pub fn has_filmstrip_decode_input(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone)]
pub struct PresentationPlanQueue {
    plans: VecDeque<BroadcastPresentationPlan>,
    capacity: usize,
}

impl PresentationPlanQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            plans: VecDeque::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
        }
    }

    pub fn len(&self) -> usize {
        self.plans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plans.is_empty()
    }

    pub fn oldest_frame(&self) -> Option<FrameNumber> {
        self.plans.front().map(|plan| plan.source_frame)
    }

    pub fn newest_frame(&self) -> Option<FrameNumber> {
        self.plans.back().map(|plan| plan.source_frame)
    }

    pub fn clear(&mut self) {
        self.plans.clear();
    }

    pub fn push_plan(&mut self, plan: BroadcastPresentationPlan) {
        while self
            .plans
            .back()
            .map(|queued| queued.source_frame >= plan.source_frame)
            .unwrap_or(false)
        {
            self.plans.pop_back();
        }

        self.plans.push_back(plan);
        while self.plans.len() > self.capacity {
            self.plans.pop_front();
        }
    }

    pub fn push_batch(&mut self, batch: BroadcastPresentationBatch) {
        for plan in batch.plans {
            self.push_plan(plan);
        }
    }
}

impl PresentationPlanQueue {
    pub fn plan_for_program_clock(
        &mut self,
        master_frame: FrameNumber,
    ) -> Option<BroadcastPresentationPlan> {
        while self.plans.len() > 1 {
            let Some(next) = self.plans.get(1) else {
                break;
            };
            if next.source_frame <= master_frame {
                self.plans.pop_front();
            } else {
                break;
            }
        }

        self.plans
            .front()
            .filter(|plan| plan.source_frame <= master_frame)
            .cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentationPlanErrorKind {
    Audio,
    Video,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentationPlanError {
    pub kind: PresentationPlanErrorKind,
    pub message: String,
}

impl PresentationPlanError {
    pub fn audio(message: impl Into<String>) -> Self {
        Self {
            kind: PresentationPlanErrorKind::Audio,
            message: message.into(),
        }
    }

    pub fn video(message: impl Into<String>) -> Self {
        Self {
            kind: PresentationPlanErrorKind::Video,
            message: message.into(),
        }
    }
}

impl From<AudioMixPlanError> for PresentationPlanError {
    fn from(value: AudioMixPlanError) -> Self {
        Self::audio(value.message)
    }
}

impl From<VideoCompositePlanError> for PresentationPlanError {
    fn from(value: VideoCompositePlanError) -> Self {
        Self::video(value.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::backend::FrameDecodePlan;
    use crate::broadcast::timebase::{FrameRange, Timebase};
    use crate::broadcast::{
        AudioChannel, AudioMix, BroadcastFrameScheduler, BroadcastProgramGraph,
        BroadcastRenderPlan, CelluloidTrack, MarkerKind, UniversalTimelineSpec, VirtualMediaRef,
    };

    #[test]
    fn presentation_plan_combines_audio_video_markers_on_same_carrier_frame() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap_vo",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        let mut spec = UniversalTimelineSpec::new(carrier);
        let cover_range = FrameRange::new(FrameNumber(25), FrameNumber(50));
        spec.add_off_vo_audio_with_mix(
            VirtualMediaRef::new("vo", "clip_vo"),
            AudioMix::with_gain_db_tenths(-60),
        );
        spec.add_cover_overlay_with_audio_mix(
            1,
            VirtualMediaRef::new("cover_video", "clip_cover_video"),
            Some(VirtualMediaRef::new("cover_audio", "clip_cover_audio")),
            cover_range,
            AudioMix::muted(),
        );
        spec.add_marker("m1", MarkerKind::M, FrameNumber(25));

        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let decode_plan = FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(25)),
        );
        let presentation = BroadcastPresentationPlan::try_from_decode_plan(&decode_plan).unwrap();

        assert_eq!(presentation.source_frame, FrameNumber(25));
        assert_eq!(presentation.pts_sec, 1.0);
        assert_eq!(presentation.video.layers.len(), 1);
        assert_eq!(presentation.audio.inputs.len(), 2);
        assert_eq!(
            presentation
                .audio
                .audible_inputs()
                .map(|input| input.channel)
                .collect::<Vec<_>>(),
            vec![AudioChannel::A1]
        );
        assert_eq!(presentation.markers.len(), 1);
        assert!(presentation.has_video_layers());
        assert!(presentation.has_audible_audio());
    }

    #[test]
    fn presentation_plan_allows_audio_only_off_vo_frame() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap_vo",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        let mut spec = UniversalTimelineSpec::new(carrier);
        spec.add_off_vo_audio(VirtualMediaRef::new("vo", "clip_vo"));

        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let decode_plan = FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(10)),
        );
        let presentation = BroadcastPresentationPlan::try_from_decode_plan(&decode_plan).unwrap();

        assert_eq!(presentation.source_frame, FrameNumber(10));
        assert!(presentation.video.layers.is_empty());
        assert_eq!(presentation.audio.inputs.len(), 1);
        assert!(!presentation.has_video_layers());
        assert!(presentation.has_audible_audio());
    }

    #[test]
    fn presentation_plan_reports_audio_sync_error() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap_vo",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        let mut spec = UniversalTimelineSpec::new(carrier);
        spec.add_off_vo_audio(VirtualMediaRef::new("vo", "clip_vo"));

        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let mut decode_plan = FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(10)),
        );
        decode_plan.audio[0].source_frame = FrameNumber(9);

        let err = BroadcastPresentationPlan::try_from_decode_plan(&decode_plan).unwrap_err();
        assert_eq!(err.kind, PresentationPlanErrorKind::Audio);
    }

    #[test]
    fn presentation_plan_rejects_filmstrip_decode_input() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        let spec = UniversalTimelineSpec::new(carrier);
        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let mut decode_plan = FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(10)),
        );
        decode_plan
            .video
            .push(crate::broadcast::VideoDecodeRequest {
                layer_id: "filmstrip:source".into(),
                role: crate::broadcast::VideoRenderRole::Base,
                z_priority: crate::broadcast::ZPriority::BASE_VIDEO,
                source_frame: decode_plan.source_frame,
                pts_sec: decode_plan.pts_sec,
                media_seek_sec: decode_plan
                    .video
                    .first()
                    .map(|request| request.media_seek_sec)
                    .unwrap_or(0.0),
                source: crate::broadcast::VideoRenderSource::VirtualShot {
                    virtual_shot_id: "shot".into(),
                    clip_id: "clip".into(),
                },
            });

        let err = BroadcastPresentationPlan::try_from_decode_plan(&decode_plan).unwrap_err();
        assert_eq!(err.kind, PresentationPlanErrorKind::Video);
        assert!(err.message.contains("filmstrip"));
    }

    #[test]
    fn presentation_batch_keeps_lookahead_frames_in_carrier_order() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap_vo",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        let mut spec = UniversalTimelineSpec::new(carrier);
        let cover_range = FrameRange::new(FrameNumber(25), FrameNumber(27));
        spec.add_off_vo_audio(VirtualMediaRef::new("vo", "clip_vo"));
        spec.add_cover_overlay(
            1,
            VirtualMediaRef::new("cover_video", "clip_cover_video"),
            Some(VirtualMediaRef::new("cover_audio", "clip_cover_audio")),
            cover_range,
        );

        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let decode_batch = crate::broadcast::FrameDecodeBatch::from_scheduler(
            &render_plan,
            &scheduler,
            FrameNumber(24),
            4,
        );
        let presentation_batch =
            BroadcastPresentationBatch::try_from_decode_batch(&decode_batch).unwrap();

        assert_eq!(presentation_batch.window.start, FrameNumber(24));
        assert_eq!(presentation_batch.len(), 4);
        assert_eq!(
            presentation_batch
                .plans
                .iter()
                .map(|plan| plan.source_frame)
                .collect::<Vec<_>>(),
            vec![
                FrameNumber(24),
                FrameNumber(25),
                FrameNumber(26),
                FrameNumber(27)
            ]
        );
        assert!(!presentation_batch.plans[0].has_video_layers());
        assert!(presentation_batch.plans[1].has_video_layers());
        assert!(presentation_batch.plans[2].has_video_layers());
        assert!(!presentation_batch.plans[3].has_video_layers());
        assert_eq!(
            presentation_batch.plans[1]
                .audio
                .inputs
                .iter()
                .map(|input| input.channel)
                .collect::<Vec<_>>(),
            vec![AudioChannel::A1, AudioChannel::A2]
        );
    }

    #[test]
    fn presentation_batch_propagates_plan_errors() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap_vo",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        let mut spec = UniversalTimelineSpec::new(carrier);
        spec.add_off_vo_audio(VirtualMediaRef::new("vo", "clip_vo"));

        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let mut decode_batch = crate::broadcast::FrameDecodeBatch::from_scheduler(
            &render_plan,
            &scheduler,
            FrameNumber(10),
            2,
        );
        decode_batch.plans[1].audio[0].pts_sec = 9.0;

        let err = BroadcastPresentationBatch::try_from_decode_batch(&decode_batch).unwrap_err();
        assert_eq!(err.kind, PresentationPlanErrorKind::Audio);
    }

    #[test]
    fn presentation_queue_returns_latest_ready_plan_for_program_clock() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap_vo",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        let mut spec = UniversalTimelineSpec::new(carrier);
        spec.add_off_vo_audio(VirtualMediaRef::new("vo", "clip_vo"));
        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let decode_batch = crate::broadcast::FrameDecodeBatch::from_scheduler(
            &render_plan,
            &scheduler,
            FrameNumber(10),
            4,
        );
        let presentation_batch =
            BroadcastPresentationBatch::try_from_decode_batch(&decode_batch).unwrap();
        let mut queue = PresentationPlanQueue::new(8);
        queue.push_batch(presentation_batch);

        assert_eq!(queue.oldest_frame(), Some(FrameNumber(10)));
        assert_eq!(queue.newest_frame(), Some(FrameNumber(13)));
        assert_eq!(
            queue
                .plan_for_program_clock(FrameNumber(9))
                .map(|plan| plan.source_frame),
            None
        );
        assert_eq!(
            queue
                .plan_for_program_clock(FrameNumber(11))
                .map(|plan| plan.source_frame),
            Some(FrameNumber(11))
        );
        assert_eq!(
            queue
                .plan_for_program_clock(FrameNumber(20))
                .map(|plan| plan.source_frame),
            Some(FrameNumber(13))
        );
    }

    #[test]
    fn presentation_queue_replaces_overlapping_future_plans() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap_vo",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        let mut spec = UniversalTimelineSpec::new(carrier);
        spec.add_off_vo_audio(VirtualMediaRef::new("vo", "clip_vo"));
        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let first_decode_batch = crate::broadcast::FrameDecodeBatch::from_scheduler(
            &render_plan,
            &scheduler,
            FrameNumber(10),
            4,
        );
        let second_decode_batch = crate::broadcast::FrameDecodeBatch::from_scheduler(
            &render_plan,
            &scheduler,
            FrameNumber(12),
            4,
        );

        let mut queue = PresentationPlanQueue::new(8);
        queue.push_batch(
            BroadcastPresentationBatch::try_from_decode_batch(&first_decode_batch).unwrap(),
        );
        queue.push_batch(
            BroadcastPresentationBatch::try_from_decode_batch(&second_decode_batch).unwrap(),
        );

        assert_eq!(queue.len(), 6);
        assert_eq!(
            queue
                .plans
                .iter()
                .map(|plan| plan.source_frame)
                .collect::<Vec<_>>(),
            vec![
                FrameNumber(10),
                FrameNumber(11),
                FrameNumber(12),
                FrameNumber(13),
                FrameNumber(14),
                FrameNumber(15)
            ]
        );
    }
}
