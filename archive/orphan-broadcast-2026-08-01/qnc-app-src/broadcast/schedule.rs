//! Frame-level broadcast scheduler.
//!
//! Given the program/reference carrier frame, this returns the active render
//! inputs for that exact frame. This is still media-backend neutral.

use super::render::{
    AudioRenderBus, BroadcastRenderPlan, EffectRenderEvent, MarkerRenderEvent, VideoRenderLayer,
};
use super::timebase::FrameNumber;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledProgramFrame {
    pub source_frame: FrameNumber,
    pub video_layers: Vec<VideoRenderLayer>,
    pub audio_buses: Vec<AudioRenderBus>,
    pub markers: Vec<MarkerRenderEvent>,
    pub effects: Vec<EffectRenderEvent>,
}

#[derive(Debug, Clone)]
pub struct BroadcastFrameScheduler {
    plan: BroadcastRenderPlan,
}

impl BroadcastFrameScheduler {
    pub fn new(plan: BroadcastRenderPlan) -> Self {
        Self { plan }
    }

    pub fn plan(&self) -> &BroadcastRenderPlan {
        &self.plan
    }

    pub fn schedule_frame(&self, frame: FrameNumber) -> ScheduledProgramFrame {
        let source_frame = self.plan.carrier.clamp_source_frame(frame);
        ScheduledProgramFrame {
            source_frame,
            video_layers: self
                .plan
                .video_layers
                .iter()
                .filter(|layer| active_at(layer.frame_range, source_frame))
                .cloned()
                .collect(),
            audio_buses: self
                .plan
                .audio_buses
                .iter()
                .filter(|bus| active_at(bus.frame_range, source_frame))
                .cloned()
                .collect(),
            markers: self
                .plan
                .markers
                .iter()
                .filter(|event| event.frame == source_frame)
                .cloned()
                .collect(),
            effects: self
                .plan
                .effects
                .iter()
                .filter(|event| event.frame == source_frame)
                .cloned()
                .collect(),
        }
    }
}

fn active_at(range: Option<super::timebase::FrameRange>, frame: FrameNumber) -> bool {
    range.map(|range| range.contains(frame)).unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::timebase::{FrameRange, Timebase};
    use crate::broadcast::{
        AudioChannel, AudioLayerSourceSpec, BroadcastProgramGraph, CelluloidTrack,
        FilmstripUnderlay, MarkerKind, UniversalTimelineSpec, VideoLayerSourceSpec,
        VirtualMediaRef,
    };

    #[test]
    fn scheduler_selects_overlay_only_inside_its_frame_range() {
        let carrier = CelluloidTrack::new(
            "project",
            "timeline",
            "clip",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        let mut spec =
            UniversalTimelineSpec::new(carrier).with_filmstrip(FilmstripUnderlay::Hidden);
        spec = spec.with_base_video(VideoLayerSourceSpec::VirtualShot(VirtualMediaRef::new(
            "base",
            "clip_base",
        )));
        spec.add_overlay_range(
            1,
            VideoLayerSourceSpec::VirtualShot(VirtualMediaRef::new("cover", "clip_cover")),
            FrameRange::new(FrameNumber(25), FrameNumber(50)),
        );
        spec.add_audio_track(AudioChannel::new(1).unwrap(), AudioLayerSourceSpec::Silence);

        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(plan);

        assert_eq!(
            scheduler.schedule_frame(FrameNumber(24)).video_layers.len(),
            1
        );
        assert_eq!(
            scheduler.schedule_frame(FrameNumber(25)).video_layers.len(),
            2
        );
        assert_eq!(
            scheduler.schedule_frame(FrameNumber(50)).video_layers.len(),
            1
        );
    }

    #[test]
    fn scheduler_emits_marker_events_at_exact_carrier_frame() {
        let carrier = CelluloidTrack::new(
            "project",
            "timeline",
            "clip",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        let mut spec = UniversalTimelineSpec::new(carrier);
        spec.add_marker("m1", MarkerKind::M, FrameNumber(42));
        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(plan);

        assert!(scheduler.schedule_frame(FrameNumber(41)).markers.is_empty());
        assert_eq!(
            scheduler.schedule_frame(FrameNumber(42)).markers[0].marker_id,
            "m1"
        );
    }

    #[test]
    fn scheduler_activates_cover_overlay_and_a2_only_inside_cover_range() {
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
            Some(VirtualMediaRef::new("cover_audio", "clip_cover_audio")),
            cover_range,
        );
        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(plan);

        let before = scheduler.schedule_frame(FrameNumber(24));
        assert!(before.video_layers.is_empty());
        assert_eq!(
            before
                .audio_buses
                .iter()
                .map(|bus| bus.channel)
                .collect::<Vec<_>>(),
            vec![AudioChannel::A1]
        );

        let inside = scheduler.schedule_frame(FrameNumber(25));
        assert_eq!(inside.video_layers.len(), 1);
        assert_eq!(
            inside
                .audio_buses
                .iter()
                .map(|bus| bus.channel)
                .collect::<Vec<_>>(),
            vec![AudioChannel::A1, AudioChannel::A2]
        );
        assert_eq!(
            inside
                .audio_buses
                .iter()
                .find(|bus| bus.channel == AudioChannel::A2)
                .unwrap()
                .mix,
            crate::broadcast::AudioMix::UNITY
        );

        let after = scheduler.schedule_frame(FrameNumber(50));
        assert!(after.video_layers.is_empty());
        assert_eq!(
            after
                .audio_buses
                .iter()
                .map(|bus| bus.channel)
                .collect::<Vec<_>>(),
            vec![AudioChannel::A1]
        );
    }
}
