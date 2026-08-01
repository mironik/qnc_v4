//! Broadcast render/decoder plan.
//!
//! This is the boundary between the editorial timeline model and the media
//! backend. Decoder workers must consume this plan, not UI filmstrip data.

use super::celluloid::CelluloidTrack;
use super::graph::BroadcastProgramGraph;
use super::layers::{
    AudioChannel, AudioMix, EffectKind, MarkerKind, ProgramLayerKind, ProgramLayerSource, ZPriority,
};
use super::timebase::{FrameNumber, FrameRange};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineUnderlay {
    None,
    Filmstrip { virtual_shot_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoRenderRole {
    Base,
    Overlay { index: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoRenderSource {
    VirtualShot {
        virtual_shot_id: String,
        clip_id: String,
    },
    Blank,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoRenderLayer {
    pub layer_id: String,
    pub role: VideoRenderRole,
    pub z_priority: ZPriority,
    pub frame_range: Option<FrameRange>,
    pub source: VideoRenderSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioRenderSource {
    VirtualShot {
        virtual_shot_id: String,
        clip_id: String,
        channel: AudioChannel,
    },
    Silence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioRenderBus {
    pub layer_id: String,
    pub channel: AudioChannel,
    pub frame_range: Option<FrameRange>,
    pub mix: AudioMix,
    pub source: AudioRenderSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerRenderEvent {
    pub marker_id: String,
    pub kind: MarkerKind,
    pub frame: FrameNumber,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectRenderEvent {
    pub marker_id: String,
    pub effect: EffectKind,
    pub frame: FrameNumber,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastRenderPlan {
    pub carrier: CelluloidTrack,
    pub underlay: TimelineUnderlay,
    pub video_layers: Vec<VideoRenderLayer>,
    pub audio_buses: Vec<AudioRenderBus>,
    pub markers: Vec<MarkerRenderEvent>,
    pub effects: Vec<EffectRenderEvent>,
}

impl BroadcastRenderPlan {
    pub fn from_graph(graph: &BroadcastProgramGraph) -> Self {
        let mut plan = Self {
            carrier: graph.carrier.clone(),
            underlay: TimelineUnderlay::None,
            video_layers: Vec::new(),
            audio_buses: Vec::new(),
            markers: Vec::new(),
            effects: Vec::new(),
        };

        for layer in &graph.layers {
            match (&layer.kind, &layer.source) {
                (
                    ProgramLayerKind::Filmstrip,
                    ProgramLayerSource::FilmstripPreview { virtual_shot_id },
                ) => {
                    plan.underlay = TimelineUnderlay::Filmstrip {
                        virtual_shot_id: virtual_shot_id.clone(),
                    };
                }
                (ProgramLayerKind::BaseVideo, source) => {
                    plan.video_layers.push(VideoRenderLayer {
                        layer_id: layer.id.clone(),
                        role: VideoRenderRole::Base,
                        z_priority: layer
                            .kind
                            .visual_z_priority()
                            .expect("base video layer must have visual Z priority"),
                        frame_range: layer.frame_range,
                        source: video_render_source(source),
                    });
                }
                (ProgramLayerKind::Overlay { index }, source) => {
                    plan.video_layers.push(VideoRenderLayer {
                        layer_id: layer.id.clone(),
                        role: VideoRenderRole::Overlay { index: *index },
                        z_priority: layer
                            .kind
                            .visual_z_priority()
                            .expect("overlay layer must have visual Z priority"),
                        frame_range: layer.frame_range,
                        source: video_render_source(source),
                    });
                }
                (ProgramLayerKind::Audio(channel), source) => {
                    plan.audio_buses.push(AudioRenderBus {
                        layer_id: layer.id.clone(),
                        channel: *channel,
                        frame_range: layer.frame_range,
                        mix: layer.audio_mix.unwrap_or_default(),
                        source: audio_render_source(*channel, source),
                    });
                }
                (
                    ProgramLayerKind::Marker(kind),
                    ProgramLayerSource::MarkerEvent {
                        marker_id, frame, ..
                    },
                ) => {
                    plan.markers.push(MarkerRenderEvent {
                        marker_id: marker_id.clone(),
                        kind: *kind,
                        frame: *frame,
                    });
                }
                (
                    ProgramLayerKind::Effect(effect),
                    ProgramLayerSource::EffectEvent {
                        marker_id, frame, ..
                    },
                ) => {
                    plan.effects.push(EffectRenderEvent {
                        marker_id: marker_id.clone(),
                        effect: *effect,
                        frame: *frame,
                    });
                }
                _ => {}
            }
        }

        plan.video_layers.sort_by_key(|layer| layer.z_priority);
        plan.audio_buses.sort_by_key(|bus| bus.channel);
        plan.markers.sort_by_key(|event| event.frame);
        plan.effects.sort_by_key(|event| event.frame);
        plan
    }

    pub fn has_filmstrip_decoder_input(&self) -> bool {
        self.video_layers.iter().any(|layer| {
            matches!(
                layer.source,
                VideoRenderSource::VirtualShot {
                    ref virtual_shot_id,
                    ..
                } if matches!(
                    &self.underlay,
                    TimelineUnderlay::Filmstrip {
                        virtual_shot_id: underlay_id
                    } if underlay_id == virtual_shot_id
                )
            ) && layer.layer_id.contains("filmstrip")
        })
    }
}

fn video_render_source(source: &ProgramLayerSource) -> VideoRenderSource {
    match source {
        ProgramLayerSource::VirtualShotVideo {
            virtual_shot_id,
            clip_id,
        } => VideoRenderSource::VirtualShot {
            virtual_shot_id: virtual_shot_id.clone(),
            clip_id: clip_id.clone(),
        },
        _ => VideoRenderSource::Blank,
    }
}

fn audio_render_source(channel: AudioChannel, source: &ProgramLayerSource) -> AudioRenderSource {
    match source {
        ProgramLayerSource::VirtualShotAudio {
            virtual_shot_id,
            clip_id,
            ..
        } => AudioRenderSource::VirtualShot {
            virtual_shot_id: virtual_shot_id.clone(),
            clip_id: clip_id.clone(),
            channel,
        },
        _ => AudioRenderSource::Silence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::timebase::{FrameRange, Timebase};
    use crate::broadcast::{
        AudioLayerSourceSpec, BroadcastPlaybackSource, BroadcastProgramGraph, CelluloidTrack,
        FilmstripUnderlay, MarkerKind, UniversalTimelineSpec, VideoLayerSourceSpec,
        VirtualMediaRef,
    };

    fn source() -> BroadcastPlaybackSource {
        BroadcastPlaybackSource {
            project_id: "project".into(),
            virtual_shot_id: "shot".into(),
            clip_id: "clip".into(),
            source_range: FrameRange::new(FrameNumber(100), FrameNumber(200)),
            source_timebase: Timebase::from_source_fps(25.0),
            has_video: true,
            has_audio: true,
            audio_channels: 2,
        }
    }

    #[test]
    fn render_plan_separates_filmstrip_underlay_from_decoder_inputs() {
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source());
        let plan = BroadcastRenderPlan::from_graph(&graph);

        assert_eq!(
            plan.underlay,
            TimelineUnderlay::Filmstrip {
                virtual_shot_id: "shot".into()
            }
        );
        assert!(!plan.has_filmstrip_decoder_input());
        assert_eq!(plan.video_layers.len(), 1);
        assert!(matches!(
            plan.video_layers[0].source,
            VideoRenderSource::VirtualShot { .. }
        ));
    }

    #[test]
    fn render_plan_routes_source_rack_to_a1_and_a2_media() {
        // Stereo/multi source: each bus gets the same virtual shot; ffmpeg
        // extracts source ch0→A1, ch1→A2 (no L+R downmix onto A1).
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&BroadcastPlaybackSource {
            audio_channels: 4,
            ..source()
        });
        let plan = BroadcastRenderPlan::from_graph(&graph);

        assert_eq!(
            plan.audio_buses
                .iter()
                .map(|bus| bus.channel.get())
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert!(plan
            .audio_buses
            .iter()
            .all(|bus| { matches!(bus.source, AudioRenderSource::VirtualShot { .. }) }));
    }

    #[test]
    fn render_plan_keeps_video_only_sources_playable_with_silence() {
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&BroadcastPlaybackSource {
            has_audio: false,
            audio_channels: 0,
            ..source()
        });
        let plan = BroadcastRenderPlan::from_graph(&graph);

        assert_eq!(plan.audio_buses.len(), 2);
        assert!(matches!(
            plan.audio_buses[0].source,
            AudioRenderSource::Silence
        ));
        assert!(matches!(
            plan.audio_buses[1].source,
            AudioRenderSource::Silence
        ));
        assert_eq!(plan.video_layers.len(), 1);
    }

    #[test]
    fn render_plan_keeps_audio_only_sources_on_carrier_without_video_layer() {
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&BroadcastPlaybackSource {
            has_video: false,
            ..source()
        });
        let plan = BroadcastRenderPlan::from_graph(&graph);

        assert_eq!(plan.video_layers.len(), 0);
        assert_eq!(plan.audio_buses.len(), 2);
        assert!(matches!(
            plan.underlay,
            TimelineUnderlay::Filmstrip { virtual_shot_id: _ }
        ));
    }

    #[test]
    fn render_plan_orders_base_and_overlay_video_layers() {
        let carrier = CelluloidTrack::new(
            "project",
            "timeline",
            "clip",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(250)),
        );
        let mut spec =
            UniversalTimelineSpec::new(carrier).with_filmstrip(FilmstripUnderlay::Hidden);
        spec = spec.with_base_video(VideoLayerSourceSpec::VirtualShot(VirtualMediaRef::new(
            "base",
            "clip_base",
        )));
        spec.add_overlay(
            1,
            VideoLayerSourceSpec::VirtualShot(VirtualMediaRef::new("cover1", "clip_cover1")),
        );
        spec.add_overlay(
            2,
            VideoLayerSourceSpec::VirtualShot(VirtualMediaRef::new("cover2", "clip_cover2")),
        );
        spec.add_audio_track(
            crate::broadcast::AudioChannel::new(1).unwrap(),
            AudioLayerSourceSpec::Silence,
        );
        spec.add_marker("m1", MarkerKind::M, FrameNumber(100));

        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let plan = BroadcastRenderPlan::from_graph(&graph);

        assert_eq!(
            plan.video_layers
                .iter()
                .map(|layer| layer.role)
                .collect::<Vec<_>>(),
            vec![
                VideoRenderRole::Base,
                VideoRenderRole::Overlay { index: 1 },
                VideoRenderRole::Overlay { index: 2 }
            ]
        );
        assert_eq!(
            plan.video_layers
                .iter()
                .map(|layer| layer.z_priority)
                .collect::<Vec<_>>(),
            vec![
                ZPriority::BASE_VIDEO,
                ZPriority::new(1_001),
                ZPriority::new(1_002)
            ]
        );
        assert_eq!(plan.markers[0].kind, MarkerKind::M);
    }

    #[test]
    fn render_plan_carries_independent_a1_and_per_cover_a2_mix() {
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
            AudioMix::with_gain_db_tenths(-120),
        );

        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let plan = BroadcastRenderPlan::from_graph(&graph);

        let a1 = plan
            .audio_buses
            .iter()
            .find(|bus| bus.channel == crate::broadcast::AudioChannel::A1)
            .unwrap();
        let a2 = plan
            .audio_buses
            .iter()
            .find(|bus| bus.channel == crate::broadcast::AudioChannel::A2)
            .unwrap();

        assert_eq!(a1.mix, AudioMix::with_gain_db_tenths(-60));
        assert_eq!(a1.frame_range, None);
        assert_eq!(a2.mix, AudioMix::with_gain_db_tenths(-120));
        assert_eq!(a2.frame_range, Some(cover_range));
    }
}
