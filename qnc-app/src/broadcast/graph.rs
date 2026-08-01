//! Broadcast program graph for the QNC/Kodak layer model.
//!
//! A graph has one celluloid/timecode carrier and many layers attached to it.
//! Rendering follows the carrier clock; no layer owns time.

use super::celluloid::CelluloidTrack;
use super::layers::{
    AudioChannel, AudioMix, EffectKind, ProgramLayer, ProgramLayerKind, ProgramLayerSource,
};
use super::timeline::{
    AudioLayerSourceSpec, FilmstripUnderlay, UniversalTimelineSpec, VideoLayerSourceSpec,
};
use super::BroadcastPlaybackSource;

#[derive(Debug, Clone)]
pub struct BroadcastProgramGraph {
    pub carrier: CelluloidTrack,
    pub layers: Vec<ProgramLayer>,
}

impl BroadcastProgramGraph {
    pub fn from_source_virtual_shot(source: &BroadcastPlaybackSource) -> Self {
        Self::from_universal_timeline(UniversalTimelineSpec::source_virtual_shot(source, true))
    }

    pub fn from_universal_timeline(spec: UniversalTimelineSpec) -> Self {
        let mut layers = Vec::new();

        if let FilmstripUnderlay::Visible { virtual_shot_id } = &spec.filmstrip {
            layers.push(ProgramLayer::new(
                "filmstrip:source",
                ProgramLayerKind::Filmstrip,
                ProgramLayerSource::FilmstripPreview {
                    virtual_shot_id: virtual_shot_id.clone(),
                },
            ));
        }

        if let Some(base_video) = &spec.base_video {
            layers.push(ProgramLayer::new(
                "video:base",
                ProgramLayerKind::BaseVideo,
                video_source_from_spec(base_video),
            ));
        }

        for (track_index, track) in spec.audio_tracks.iter().enumerate() {
            let mut layer = ProgramLayer::new(
                format!("audio:{}:{}", track.channel.get(), track_index),
                ProgramLayerKind::Audio(track.channel),
                match &track.source {
                    AudioLayerSourceSpec::VirtualShot(media) => {
                        ProgramLayerSource::VirtualShotAudio {
                            virtual_shot_id: media.virtual_shot_id.clone(),
                            clip_id: media.clip_id.clone(),
                            channel: track.channel,
                        }
                    }
                    AudioLayerSourceSpec::Silence => ProgramLayerSource::Silence {
                        channel: track.channel,
                    },
                },
            );
            layer = layer.with_audio_mix(track.mix);
            if let Some(range) = track.frame_range {
                layer = layer.with_frame_range(range);
            }
            layers.push(layer);
        }

        for overlay in &spec.overlays {
            let mut layer = ProgramLayer::new(
                format!("overlay:{}", overlay.index),
                ProgramLayerKind::Overlay {
                    index: overlay.index,
                },
                video_source_from_spec(&overlay.source),
            );
            if let Some(range) = overlay.frame_range {
                layer = layer.with_frame_range(range);
            }
            layers.push(layer);
        }

        for marker in &spec.markers {
            layers.push(ProgramLayer::new(
                format!("marker:{:?}:{}", marker.kind, marker.marker_id),
                ProgramLayerKind::Marker(marker.kind),
                ProgramLayerSource::MarkerEvent {
                    kind: marker.kind,
                    marker_id: marker.marker_id.clone(),
                    frame: marker.frame,
                },
            ));
        }

        Self {
            carrier: spec.carrier,
            layers,
        }
    }

    pub fn without_filmstrip(mut self) -> Self {
        self.layers
            .retain(|layer| !matches!(layer.kind, ProgramLayerKind::Filmstrip));
        self
    }

    pub fn add_marker(
        &mut self,
        marker_id: impl Into<String>,
        kind: super::layers::MarkerKind,
        frame: super::timebase::FrameNumber,
    ) {
        let marker_id = marker_id.into();
        self.layers.push(ProgramLayer::new(
            format!("marker:{kind:?}:{marker_id}"),
            ProgramLayerKind::Marker(kind),
            ProgramLayerSource::MarkerEvent {
                kind,
                marker_id,
                frame,
            },
        ));
    }

    pub fn add_overlay_virtual_shot(
        &mut self,
        index: u8,
        virtual_shot_id: impl Into<String>,
        clip_id: impl Into<String>,
    ) {
        let index = index.max(1);
        self.layers.push(ProgramLayer::new(
            format!("overlay:{index}"),
            ProgramLayerKind::Overlay { index },
            ProgramLayerSource::VirtualShotVideo {
                virtual_shot_id: virtual_shot_id.into(),
                clip_id: clip_id.into(),
            },
        ));
    }

    pub fn add_marker_effect(&mut self, marker_id: impl Into<String>) {
        let marker_id = marker_id.into();
        let frame = self.carrier.source_range.start;
        self.layers.push(ProgramLayer::new(
            format!("effect:marker:{marker_id}"),
            ProgramLayerKind::Effect(EffectKind::Marker),
            ProgramLayerSource::EffectEvent {
                effect: EffectKind::Marker,
                marker_id,
                frame,
            },
        ));
    }

    pub fn playback_layers(&self) -> impl Iterator<Item = &ProgramLayer> {
        self.layers.iter().filter(|layer| layer.is_playback_media())
    }

    pub fn has_filmstrip_playback_source(&self) -> bool {
        self.playback_layers()
            .any(|layer| layer.source.is_filmstrip_preview())
    }
}

fn video_source_from_spec(source: &VideoLayerSourceSpec) -> ProgramLayerSource {
    match source {
        VideoLayerSourceSpec::VirtualShot(media) => ProgramLayerSource::VirtualShotVideo {
            virtual_shot_id: media.virtual_shot_id.clone(),
            clip_id: media.clip_id.clone(),
        },
        VideoLayerSourceSpec::Blank => ProgramLayerSource::BlankVideo,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::timebase::{FrameNumber, FrameRange, Timebase};

    fn source(has_audio: bool, audio_channels: u8, has_video: bool) -> BroadcastPlaybackSource {
        BroadcastPlaybackSource {
            project_id: "project".into(),
            virtual_shot_id: "shot".into(),
            clip_id: "clip".into(),
            source_range: FrameRange::new(FrameNumber(100), FrameNumber(200)),
            source_timebase: Timebase::from_source_fps(25.0),
            has_video,
            has_audio,
            audio_channels,
        }
    }

    #[test]
    fn graph_has_one_celluloid_carrier_and_layers_on_top() {
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source(true, 2, true));

        assert_eq!(graph.carrier.virtual_shot_id, "shot");
        assert_eq!(graph.carrier.source_range.start, FrameNumber(100));
        assert!(graph
            .layers
            .iter()
            .any(|layer| matches!(layer.kind, ProgramLayerKind::Filmstrip)));
        assert!(graph
            .layers
            .iter()
            .any(|layer| matches!(layer.kind, ProgramLayerKind::BaseVideo)));
    }

    #[test]
    fn filmstrip_is_never_a_playback_source() {
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source(true, 1, true));

        assert!(!graph.has_filmstrip_playback_source());
        assert!(graph
            .layers
            .iter()
            .any(|layer| layer.source.is_filmstrip_preview()));
    }

    #[test]
    fn video_without_audio_gets_silence_layer_not_clock_failure() {
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source(false, 0, true));

        assert!(graph.layers.iter().any(|layer| {
            matches!(
                layer.source,
                ProgramLayerSource::Silence {
                    channel
                } if channel.get() == 1
            )
        }));
    }

    #[test]
    fn audio_only_source_has_carrier_and_audio_but_no_base_video_layer() {
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source(true, 1, false));

        assert_eq!(graph.carrier.virtual_shot_id, "shot");
        assert!(graph
            .layers
            .iter()
            .any(|layer| matches!(layer.kind, ProgramLayerKind::Audio(_))));
        assert!(!graph
            .layers
            .iter()
            .any(|layer| matches!(layer.kind, ProgramLayerKind::BaseVideo)));
        assert!(graph
            .playback_layers()
            .all(|layer| !matches!(layer.kind, ProgramLayerKind::BaseVideo)));
    }

    #[test]
    fn off_vo_uses_a1_and_cover_uses_overlay_plus_a2() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap_vo",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(250)),
        );
        let mut spec = UniversalTimelineSpec::new(carrier);
        let cover_range = FrameRange::new(FrameNumber(25), FrameNumber(75));
        spec.add_off_vo_audio(super::super::timeline::VirtualMediaRef::new(
            "vo", "clip_vo",
        ));
        spec.add_cover_overlay_with_audio_mix(
            1,
            super::super::timeline::VirtualMediaRef::new("cover_video", "clip_cover_video"),
            Some(super::super::timeline::VirtualMediaRef::new(
                "cover_audio",
                "clip_cover_audio",
            )),
            cover_range,
            AudioMix::with_gain_db_tenths(-90),
        );

        let graph = BroadcastProgramGraph::from_universal_timeline(spec);

        assert!(!graph
            .layers
            .iter()
            .any(|layer| matches!(layer.kind, ProgramLayerKind::BaseVideo)));
        assert!(graph
            .layers
            .iter()
            .any(|layer| matches!(layer.kind, ProgramLayerKind::Overlay { index: 1 })));
        assert!(graph.layers.iter().any(|layer| {
            matches!(
                layer.kind,
                ProgramLayerKind::Audio(channel) if channel == AudioChannel::A1
            ) && layer.frame_range.is_none()
        }));
        assert!(graph.layers.iter().any(|layer| {
            matches!(
                layer.kind,
                ProgramLayerKind::Audio(channel) if channel == AudioChannel::A2
            ) && layer.frame_range == Some(cover_range)
                && layer.audio_mix == Some(AudioMix::with_gain_db_tenths(-90))
        }));
    }

    #[test]
    fn source_program_rack_maps_media_channels_to_a1_through_a4() {
        // Source open builds one mono bus per media channel (pad to A1+A2).
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source(true, 9, true));
        let audio: Vec<_> = graph
            .layers
            .iter()
            .filter_map(|layer| match layer.kind {
                ProgramLayerKind::Audio(ch) => Some((ch, &layer.source)),
                _ => None,
            })
            .collect();
        assert_eq!(audio.len(), 4);
        assert_eq!(audio[0].0, AudioChannel::A1);
        assert_eq!(audio[1].0, AudioChannel::A2);
        assert_eq!(audio[2].0, AudioChannel::A3);
        assert_eq!(audio[3].0, AudioChannel::A4);
        assert!(audio
            .iter()
            .all(|(_, source)| { matches!(source, ProgramLayerSource::VirtualShotAudio { .. }) }));
        assert!(
            source(true, 9, true).normalized_audio_channels() <= 4,
            "channel hint still capped at A4 max"
        );
    }

    #[test]
    fn overlay_and_marker_are_regular_layers_on_same_carrier() {
        let mut graph = BroadcastProgramGraph::from_source_virtual_shot(&source(true, 1, true));
        graph.add_overlay_virtual_shot(1, "cover_shot", "cover_clip");
        graph.add_marker_effect("m1");

        assert!(graph
            .layers
            .iter()
            .any(|layer| matches!(layer.kind, ProgramLayerKind::Overlay { index: 1 })));
        assert!(graph
            .layers
            .iter()
            .any(|layer| matches!(layer.kind, ProgramLayerKind::Effect(EffectKind::Marker))));
        assert_eq!(graph.carrier.virtual_shot_id, "shot");
    }
}
