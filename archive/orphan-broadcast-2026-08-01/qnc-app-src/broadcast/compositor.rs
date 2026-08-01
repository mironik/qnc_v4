//! Broadcast video compositing decision plan.
//!
//! Video compositing uses only visual layers ordered by `ZPriority`. Timecode
//! carrier and audio buses are not part of the visual Z axis. Base video is
//! optional: an OFF/VO story can have no video layers until a cover/overlay is
//! active.

use super::backend::FrameDecodePlan;
use super::layers::ZPriority;
use super::render::{VideoRenderRole, VideoRenderSource};
use super::timebase::FrameNumber;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCompositeRole {
    Base,
    Overlay { index: u8 },
}

impl From<VideoRenderRole> for VideoCompositeRole {
    fn from(role: VideoRenderRole) -> Self {
        match role {
            VideoRenderRole::Base => Self::Base,
            VideoRenderRole::Overlay { index } => Self::Overlay { index },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoCompositeLayer {
    pub layer_id: String,
    pub role: VideoCompositeRole,
    pub z_priority: ZPriority,
    pub source_frame: FrameNumber,
    pub pts_sec: f64,
    pub source: VideoRenderSource,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoCompositePlan {
    pub source_frame: FrameNumber,
    pub pts_sec: f64,
    pub layers: Vec<VideoCompositeLayer>,
}

impl VideoCompositePlan {
    pub fn try_from_decode_plan(plan: &FrameDecodePlan) -> Result<Self, VideoCompositePlanError> {
        for request in &plan.video {
            if request.source_frame != plan.source_frame {
                return Err(VideoCompositePlanError::new(format!(
                    "video request '{}' is on frame {:?}, expected {:?}",
                    request.layer_id, request.source_frame, plan.source_frame
                )));
            }
            if request.pts_sec != plan.pts_sec {
                return Err(VideoCompositePlanError::new(format!(
                    "video request '{}' is on PTS {}, expected {}",
                    request.layer_id, request.pts_sec, plan.pts_sec
                )));
            }
        }

        let mut layers = plan
            .video
            .iter()
            .map(|request| VideoCompositeLayer {
                layer_id: request.layer_id.clone(),
                role: VideoCompositeRole::from(request.role),
                z_priority: request.z_priority,
                source_frame: request.source_frame,
                pts_sec: request.pts_sec,
                source: request.source.clone(),
            })
            .collect::<Vec<_>>();
        layers.sort_by_key(|layer| layer.z_priority);

        Ok(Self {
            source_frame: plan.source_frame,
            pts_sec: plan.pts_sec,
            layers,
        })
    }

    pub fn has_base_video(&self) -> bool {
        self.layers
            .iter()
            .any(|layer| matches!(layer.role, VideoCompositeRole::Base))
    }

    pub fn overlays(&self) -> impl Iterator<Item = &VideoCompositeLayer> {
        self.layers
            .iter()
            .filter(|layer| matches!(layer.role, VideoCompositeRole::Overlay { .. }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoCompositePlanError {
    pub message: String,
}

impl VideoCompositePlanError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::backend::FrameDecodePlan;
    use crate::broadcast::timebase::{FrameRange, Timebase};
    use crate::broadcast::{
        AudioMix, BroadcastFrameScheduler, BroadcastProgramGraph, BroadcastRenderPlan,
        CelluloidTrack, UniversalTimelineSpec, VideoLayerSourceSpec, VirtualMediaRef,
    };

    #[test]
    fn composite_plan_allows_off_vo_without_any_video_layer() {
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
        let decode_plan = FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(10)),
        );
        let composite_plan = VideoCompositePlan::try_from_decode_plan(&decode_plan).unwrap();

        assert_eq!(composite_plan.source_frame, FrameNumber(10));
        assert!(composite_plan.layers.is_empty());
        assert!(!composite_plan.has_base_video());
    }

    #[test]
    fn composite_plan_allows_cover_overlay_without_base_video() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap_vo",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        let mut spec = UniversalTimelineSpec::new(carrier);
        let cover_range = FrameRange::new(FrameNumber(25), FrameNumber(50));
        spec.add_off_vo_audio(VirtualMediaRef::new("vo", "clip_vo"));
        spec.add_cover_overlay(
            1,
            VirtualMediaRef::new("cover_video", "clip_cover_video"),
            None,
            cover_range,
        );

        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let decode_plan = FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(25)),
        );
        let composite_plan = VideoCompositePlan::try_from_decode_plan(&decode_plan).unwrap();

        assert!(!composite_plan.has_base_video());
        assert_eq!(composite_plan.layers.len(), 1);
        assert_eq!(
            composite_plan.layers[0].role,
            VideoCompositeRole::Overlay { index: 1 }
        );
        assert_eq!(composite_plan.layers[0].z_priority, ZPriority::new(1_001));
    }

    #[test]
    fn composite_plan_orders_base_and_overlays_by_visual_z_priority() {
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
        spec.add_cover_overlay(
            2,
            VirtualMediaRef::new("cover_2", "clip_cover_2"),
            None,
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        spec.add_cover_overlay(
            1,
            VirtualMediaRef::new("cover_1", "clip_cover_1"),
            None,
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );

        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let decode_plan =
            FrameDecodePlan::from_scheduled(&render_plan, scheduler.schedule_frame(FrameNumber(0)));
        let composite_plan = VideoCompositePlan::try_from_decode_plan(&decode_plan).unwrap();

        assert!(composite_plan.has_base_video());
        assert_eq!(
            composite_plan
                .layers
                .iter()
                .map(|layer| layer.role)
                .collect::<Vec<_>>(),
            vec![
                VideoCompositeRole::Base,
                VideoCompositeRole::Overlay { index: 1 },
                VideoCompositeRole::Overlay { index: 2 }
            ]
        );
    }

    #[test]
    fn composite_plan_rejects_video_not_on_carrier_frame() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        let spec = UniversalTimelineSpec::new(carrier).with_base_video(
            VideoLayerSourceSpec::VirtualShot(VirtualMediaRef::new("base", "clip_base")),
        );
        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let mut decode_plan = FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(25)),
        );
        decode_plan.video[0].source_frame = FrameNumber(24);

        let err = VideoCompositePlan::try_from_decode_plan(&decode_plan).unwrap_err();
        assert!(err.message.contains("expected"));
    }
}
