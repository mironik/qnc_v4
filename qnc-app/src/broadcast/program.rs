//! Neutral Kodak program presets → [`UniversalTimelineSpec`].
//!
//! These helpers know **layers** (base / overlay / audio / markers). They do
//! not know Story, Wrap, Ingest, or any form. Forms build their own input and
//! pass a finished spec into the broadcast player.
//!
//! Filmstrip is **not** part of player programs — it belongs to timeline UI.

use super::celluloid::CelluloidTrack;
use super::layers::MarkerKind;
use super::timebase::{FrameNumber, FrameRange, Timebase};
use super::timeline::{
    FilmstripUnderlay, UniversalTimelineSpec, VideoLayerSourceSpec, VirtualMediaRef,
};
use super::BroadcastPlaybackSource;

/// Single virtual-shot program: BaseVideo + source audio buses (ch0→A1, ch1→A2, …).
/// Never attaches a filmstrip underlay (timeline UI owns that).
pub fn build_source_timeline(source: &BroadcastPlaybackSource) -> UniversalTimelineSpec {
    let mut spec = UniversalTimelineSpec::source_virtual_shot(source, false);
    spec.filmstrip = FilmstripUnderlay::Hidden;
    spec
}

#[derive(Debug, Clone)]
pub struct ProgramOverlayInput {
    pub overlay_index: u8,
    pub video: VirtualMediaRef,
    pub audio: Option<VirtualMediaRef>,
    pub frame_range: FrameRange,
}

#[derive(Debug, Clone)]
pub struct ProgramMarkerInput {
    pub marker_id: String,
    pub kind: MarkerKind,
    pub frame: FrameNumber,
}

/// Layered program: optional base (or blank / none) + overlays + markers.
#[derive(Debug, Clone)]
pub struct LayeredProgramInput {
    pub project_id: String,
    pub program_id: String,
    pub clip_id: String,
    pub timebase: Timebase,
    pub carrier_range: FrameRange,
    /// No base video layer (audio-only / VO-style program).
    pub omit_base_video: bool,
    pub force_blank_base: bool,
    pub base_media: Option<VirtualMediaRef>,
    pub has_base_audio: bool,
    pub overlays: Vec<ProgramOverlayInput>,
    pub markers: Vec<ProgramMarkerInput>,
}

pub fn build_layered_program(input: LayeredProgramInput) -> UniversalTimelineSpec {
    let timebase = input.timebase;
    let carrier_range = input.carrier_range;
    let carrier = CelluloidTrack::new(
        input.project_id.clone(),
        input.program_id.clone(),
        input.clip_id.clone(),
        timebase,
        carrier_range,
    );
    let mut spec = UniversalTimelineSpec::new(carrier).with_filmstrip(FilmstripUnderlay::Hidden);

    if input.omit_base_video {
        if input.force_blank_base {
            spec = spec.with_base_video(VideoLayerSourceSpec::Blank);
        }
        if let Some(media) = input.base_media {
            spec.add_off_vo_audio(media);
        }
    } else if let Some(media) = input.base_media {
        let source = BroadcastPlaybackSource {
            project_id: input.project_id,
            virtual_shot_id: media.virtual_shot_id.clone(),
            clip_id: media.clip_id.clone(),
            source_range: carrier_range,
            source_timebase: timebase,
            has_video: true,
            has_audio: input.has_base_audio,
            audio_channels: if input.has_base_audio { 2 } else { 0 },
        };
        let mut from_source = UniversalTimelineSpec::source_virtual_shot(&source, false);
        from_source.carrier = spec.carrier.clone();
        from_source.filmstrip = FilmstripUnderlay::Hidden;
        from_source.overlays.clear();
        from_source.markers.clear();
        spec = from_source;
    }

    for overlay in input.overlays {
        spec.add_cover_overlay(
            overlay.overlay_index,
            overlay.video,
            overlay.audio,
            overlay.frame_range,
        );
    }
    for marker in input.markers {
        spec.add_marker(marker.marker_id, marker.kind, marker.frame);
    }

    // Player contract: filmstrip never enters the program graph.
    spec.filmstrip = FilmstripUnderlay::Hidden;
    spec
}

/// Default open program for a probed single-shot source (no filmstrip).
pub fn source_spec_for_playback(source: &BroadcastPlaybackSource) -> UniversalTimelineSpec {
    build_source_timeline(source)
}

/// Strip any filmstrip underlay — player never owns timeline UI layers.
pub fn strip_filmstrip(mut spec: UniversalTimelineSpec) -> UniversalTimelineSpec {
    spec.filmstrip = FilmstripUnderlay::Hidden;
    spec
}
