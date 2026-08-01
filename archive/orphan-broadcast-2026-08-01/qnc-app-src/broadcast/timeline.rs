//! Universal timeline specification.
//!
//! Source and wrap timelines are not separate component types. They are layer
//! presets over the same transparent celluloid/timecode carrier.

use super::celluloid::CelluloidTrack;
use super::layers::{AudioChannel, AudioMix, MarkerKind, MAX_AUDIO_CHANNELS};
use super::timebase::{FrameNumber, FrameRange};
use super::BroadcastPlaybackSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilmstripUnderlay {
    Hidden,
    Visible { virtual_shot_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualMediaRef {
    pub virtual_shot_id: String,
    pub clip_id: String,
}

impl VirtualMediaRef {
    pub fn new(virtual_shot_id: impl Into<String>, clip_id: impl Into<String>) -> Self {
        Self {
            virtual_shot_id: virtual_shot_id.into(),
            clip_id: clip_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoLayerSourceSpec {
    VirtualShot(VirtualMediaRef),
    Blank,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioLayerSourceSpec {
    VirtualShot(VirtualMediaRef),
    Silence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrackSpec {
    pub channel: AudioChannel,
    pub source: AudioLayerSourceSpec,
    pub frame_range: Option<FrameRange>,
    pub mix: AudioMix,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayLayerSpec {
    pub index: u8,
    pub source: VideoLayerSourceSpec,
    pub frame_range: Option<FrameRange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineMarkerSpec {
    pub marker_id: String,
    pub kind: MarkerKind,
    pub frame: FrameNumber,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UniversalTimelineSpec {
    pub carrier: CelluloidTrack,
    pub filmstrip: FilmstripUnderlay,
    pub base_video: Option<VideoLayerSourceSpec>,
    pub audio_tracks: Vec<AudioTrackSpec>,
    pub overlays: Vec<OverlayLayerSpec>,
    pub markers: Vec<TimelineMarkerSpec>,
}

impl UniversalTimelineSpec {
    pub fn new(carrier: CelluloidTrack) -> Self {
        Self {
            carrier,
            filmstrip: FilmstripUnderlay::Hidden,
            base_video: None,
            audio_tracks: Vec::new(),
            overlays: Vec::new(),
            markers: Vec::new(),
        }
    }

    pub fn source_virtual_shot(source: &BroadcastPlaybackSource, show_filmstrip: bool) -> Self {
        let carrier = CelluloidTrack::new(
            source.project_id.clone(),
            source.virtual_shot_id.clone(),
            source.clip_id.clone(),
            source.source_timebase,
            source.source_range,
        );
        let media = VirtualMediaRef::new(source.virtual_shot_id.clone(), source.clip_id.clone());
        let mut spec = Self::new(carrier);
        spec.filmstrip = if show_filmstrip {
            FilmstripUnderlay::Visible {
                virtual_shot_id: source.virtual_shot_id.clone(),
            }
        } else {
            FilmstripUnderlay::Hidden
        };
        if source.has_video {
            spec.base_video = Some(VideoLayerSourceSpec::VirtualShot(media.clone()));
        }
        spec.add_source_audio_tracks(source, media);
        spec.add_marker("in", MarkerKind::In, source.source_range.start);
        spec.add_marker("out", MarkerKind::Out, source.source_range.end_exclusive);
        spec
    }

    pub fn with_filmstrip(mut self, filmstrip: FilmstripUnderlay) -> Self {
        self.filmstrip = filmstrip;
        self
    }

    pub fn with_base_video(mut self, source: VideoLayerSourceSpec) -> Self {
        self.base_video = Some(source);
        self
    }

    pub fn without_base_video(mut self) -> Self {
        self.base_video = None;
        self
    }

    pub fn add_audio_track(&mut self, channel: AudioChannel, source: AudioLayerSourceSpec) {
        self.audio_tracks.push(AudioTrackSpec {
            channel,
            source,
            frame_range: None,
            mix: AudioMix::UNITY,
        });
    }

    pub fn add_audio_track_with_mix(
        &mut self,
        channel: AudioChannel,
        source: AudioLayerSourceSpec,
        mix: AudioMix,
    ) {
        self.audio_tracks.push(AudioTrackSpec {
            channel,
            source,
            frame_range: None,
            mix,
        });
    }

    pub fn add_audio_track_range(
        &mut self,
        channel: AudioChannel,
        source: AudioLayerSourceSpec,
        frame_range: FrameRange,
    ) {
        self.audio_tracks.push(AudioTrackSpec {
            channel,
            source,
            frame_range: Some(frame_range),
            mix: AudioMix::UNITY,
        });
    }

    pub fn add_audio_track_range_with_mix(
        &mut self,
        channel: AudioChannel,
        source: AudioLayerSourceSpec,
        frame_range: FrameRange,
        mix: AudioMix,
    ) {
        self.audio_tracks.push(AudioTrackSpec {
            channel,
            source,
            frame_range: Some(frame_range),
            mix,
        });
    }

    pub fn add_off_vo_audio(&mut self, media: VirtualMediaRef) {
        self.add_off_vo_audio_with_mix(media, AudioMix::UNITY);
    }

    pub fn add_off_vo_audio_with_mix(&mut self, media: VirtualMediaRef, a1_mix: AudioMix) {
        self.add_audio_track_with_mix(
            AudioChannel::A1,
            AudioLayerSourceSpec::VirtualShot(media),
            a1_mix,
        );
    }

    pub fn add_overlay(&mut self, index: u8, source: VideoLayerSourceSpec) {
        self.overlays.push(OverlayLayerSpec {
            index: index.max(1),
            source,
            frame_range: None,
        });
    }

    pub fn add_overlay_range(
        &mut self,
        index: u8,
        source: VideoLayerSourceSpec,
        frame_range: FrameRange,
    ) {
        self.overlays.push(OverlayLayerSpec {
            index: index.max(1),
            source,
            frame_range: Some(frame_range),
        });
    }

    pub fn add_cover_overlay(
        &mut self,
        index: u8,
        video: VirtualMediaRef,
        audio: Option<VirtualMediaRef>,
        frame_range: FrameRange,
    ) {
        self.add_overlay_range(index, VideoLayerSourceSpec::VirtualShot(video), frame_range);
        if let Some(audio) = audio {
            self.add_audio_track_range_with_mix(
                AudioChannel::A2,
                AudioLayerSourceSpec::VirtualShot(audio),
                frame_range,
                AudioMix::UNITY,
            );
        }
    }

    pub fn add_cover_overlay_with_audio_mix(
        &mut self,
        index: u8,
        video: VirtualMediaRef,
        audio: Option<VirtualMediaRef>,
        frame_range: FrameRange,
        a2_mix: AudioMix,
    ) {
        self.add_overlay_range(index, VideoLayerSourceSpec::VirtualShot(video), frame_range);
        if let Some(audio) = audio {
            self.add_audio_track_range_with_mix(
                AudioChannel::A2,
                AudioLayerSourceSpec::VirtualShot(audio),
                frame_range,
                a2_mix,
            );
        }
    }

    pub fn add_marker(
        &mut self,
        marker_id: impl Into<String>,
        kind: MarkerKind,
        frame: FrameNumber,
    ) {
        self.markers.push(TimelineMarkerSpec {
            marker_id: marker_id.into(),
            kind,
            frame,
        });
    }

    fn add_source_audio_tracks(
        &mut self,
        source: &BroadcastPlaybackSource,
        media: VirtualMediaRef,
    ) {
        // Independent mono buses on the carrier — never a stereo pair decode.
        // Source channel N (0-based) maps to bus A(N+1); ffmpeg extracts that
        // channel only (no L+R downmix onto A1). Pad to at least A1+A2.
        let media_n = if source.has_audio {
            source.audio_channels.clamp(1, MAX_AUDIO_CHANNELS)
        } else {
            0
        };
        let bus_n = source.program_audio_buses();
        for index in 1..=bus_n {
            let channel = AudioChannel::new(index).expect("program bus within A1–A4");
            if index <= media_n {
                self.add_audio_track(channel, AudioLayerSourceSpec::VirtualShot(media.clone()));
            } else {
                self.add_audio_track(channel, AudioLayerSourceSpec::Silence);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::timebase::{FrameRange, Timebase};

    fn source(show_audio: bool) -> BroadcastPlaybackSource {
        BroadcastPlaybackSource {
            project_id: "project".into(),
            virtual_shot_id: "shot".into(),
            clip_id: "clip".into(),
            source_range: FrameRange::new(FrameNumber(10), FrameNumber(20)),
            source_timebase: Timebase::from_source_fps(25.0),
            has_video: true,
            has_audio: show_audio,
            audio_channels: if show_audio { 2 } else { 0 },
        }
    }

    #[test]
    fn source_timeline_is_universal_spec_preset() {
        let spec = UniversalTimelineSpec::source_virtual_shot(&source(true), true);

        assert!(matches!(spec.filmstrip, FilmstripUnderlay::Visible { .. }));
        assert!(matches!(
            spec.base_video,
            Some(VideoLayerSourceSpec::VirtualShot(_))
        ));
        assert_eq!(spec.audio_tracks.len(), 2);
        assert!(spec.markers.iter().any(|m| m.kind == MarkerKind::In));
        assert!(spec.markers.iter().any(|m| m.kind == MarkerKind::Out));
    }

    #[test]
    fn source_timeline_can_hide_filmstrip_underlay() {
        let spec = UniversalTimelineSpec::source_virtual_shot(&source(true), false);

        assert_eq!(spec.filmstrip, FilmstripUnderlay::Hidden);
    }

    #[test]
    fn source_with_stereo_audio_uses_a1_and_a2_media() {
        let spec = UniversalTimelineSpec::source_virtual_shot(&source(true), true);

        assert_eq!(spec.audio_tracks.len(), 2);
        assert_eq!(spec.audio_tracks[0].channel, AudioChannel::A1);
        assert!(matches!(
            spec.audio_tracks[0].source,
            AudioLayerSourceSpec::VirtualShot(_)
        ));
        assert_eq!(spec.audio_tracks[1].channel, AudioChannel::A2);
        assert!(matches!(
            spec.audio_tracks[1].source,
            AudioLayerSourceSpec::VirtualShot(_)
        ));
    }

    #[test]
    fn source_with_mono_audio_keeps_a2_silence() {
        let mut mono = source(true);
        mono.audio_channels = 1;
        let spec = UniversalTimelineSpec::source_virtual_shot(&mono, true);

        assert_eq!(spec.audio_tracks.len(), 2);
        assert!(matches!(
            spec.audio_tracks[0].source,
            AudioLayerSourceSpec::VirtualShot(_)
        ));
        assert!(matches!(
            spec.audio_tracks[1].source,
            AudioLayerSourceSpec::Silence
        ));
    }

    #[test]
    fn source_without_audio_gets_silence_audio_tracks_a1_a2() {
        let spec = UniversalTimelineSpec::source_virtual_shot(&source(false), true);

        assert_eq!(spec.audio_tracks.len(), 2);
        assert_eq!(spec.audio_tracks[0].channel, AudioChannel::A1);
        assert_eq!(spec.audio_tracks[1].channel, AudioChannel::A2);
        assert!(matches!(
            spec.audio_tracks[0].source,
            AudioLayerSourceSpec::Silence
        ));
        assert!(matches!(
            spec.audio_tracks[1].source,
            AudioLayerSourceSpec::Silence
        ));
    }

    #[test]
    fn audio_only_source_has_no_implicit_base_video() {
        let mut source = source(true);
        source.has_video = false;
        let spec = UniversalTimelineSpec::source_virtual_shot(&source, true);

        assert_eq!(spec.base_video, None);
        assert_eq!(spec.audio_tracks.len(), 2);
        assert!(matches!(spec.filmstrip, FilmstripUnderlay::Visible { .. }));
    }

    #[test]
    fn wrap_vo_can_start_as_timecode_plus_audio_without_video() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap_vo",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(250)),
        );
        let mut spec = UniversalTimelineSpec::new(carrier);
        spec.add_off_vo_audio(VirtualMediaRef::new("vo", "clip_vo"));

        assert_eq!(spec.base_video, None);
        assert!(spec.overlays.is_empty());
        assert_eq!(spec.audio_tracks.len(), 1);
        assert_eq!(spec.audio_tracks[0].channel, AudioChannel::A1);
        assert_eq!(spec.audio_tracks[0].frame_range, None);
    }

    #[test]
    fn off_vo_a1_audio_can_be_muted_or_lowered() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap_vo",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(250)),
        );
        let mut lowered_spec = UniversalTimelineSpec::new(carrier.clone());
        lowered_spec.add_off_vo_audio_with_mix(
            VirtualMediaRef::new("vo", "clip_vo"),
            AudioMix::with_gain_db_tenths(-60),
        );

        assert_eq!(lowered_spec.audio_tracks[0].channel, AudioChannel::A1);
        assert_eq!(lowered_spec.audio_tracks[0].frame_range, None);
        assert_eq!(lowered_spec.audio_tracks[0].mix.gain_db_tenths(), -60);
        assert!(!lowered_spec.audio_tracks[0].mix.is_muted());

        let mut muted_spec = UniversalTimelineSpec::new(carrier);
        muted_spec
            .add_off_vo_audio_with_mix(VirtualMediaRef::new("vo", "clip_vo"), AudioMix::muted());

        assert_eq!(muted_spec.audio_tracks[0].channel, AudioChannel::A1);
        assert!(muted_spec.audio_tracks[0].mix.is_muted());
    }

    #[test]
    fn cover_overlay_adds_video_overlay_and_a2_audio_range() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap_vo",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(250)),
        );
        let mut spec = UniversalTimelineSpec::new(carrier);
        let cover_range = FrameRange::new(FrameNumber(25), FrameNumber(75));
        spec.add_off_vo_audio(VirtualMediaRef::new("vo", "clip_vo"));
        spec.add_cover_overlay(
            1,
            VirtualMediaRef::new("cover_video", "clip_cover_video"),
            Some(VirtualMediaRef::new("cover_audio", "clip_cover_audio")),
            cover_range,
        );

        assert_eq!(spec.base_video, None);
        assert_eq!(spec.overlays.len(), 1);
        assert_eq!(spec.overlays[0].index, 1);
        assert_eq!(spec.overlays[0].frame_range, Some(cover_range));
        assert_eq!(spec.audio_tracks.len(), 2);
        assert_eq!(spec.audio_tracks[0].channel, AudioChannel::A1);
        assert_eq!(spec.audio_tracks[0].frame_range, None);
        assert_eq!(spec.audio_tracks[0].mix, AudioMix::UNITY);
        assert_eq!(spec.audio_tracks[1].channel, AudioChannel::A2);
        assert_eq!(spec.audio_tracks[1].frame_range, Some(cover_range));
        assert_eq!(spec.audio_tracks[1].mix, AudioMix::UNITY);
    }

    #[test]
    fn cover_overlay_can_mute_or_lower_its_own_a2_audio() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap_vo",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(250)),
        );
        let mut muted_spec = UniversalTimelineSpec::new(carrier.clone());
        let cover_range = FrameRange::new(FrameNumber(25), FrameNumber(75));
        muted_spec.add_cover_overlay_with_audio_mix(
            1,
            VirtualMediaRef::new("cover_video", "clip_cover_video"),
            Some(VirtualMediaRef::new("cover_audio", "clip_cover_audio")),
            cover_range,
            AudioMix::muted(),
        );

        assert_eq!(muted_spec.audio_tracks[0].channel, AudioChannel::A2);
        assert_eq!(muted_spec.audio_tracks[0].frame_range, Some(cover_range));
        assert!(muted_spec.audio_tracks[0].mix.is_muted());

        let mut lowered_spec = UniversalTimelineSpec::new(carrier);
        lowered_spec.add_cover_overlay_with_audio_mix(
            1,
            VirtualMediaRef::new("cover_video", "clip_cover_video"),
            Some(VirtualMediaRef::new("cover_audio", "clip_cover_audio")),
            cover_range,
            AudioMix::with_gain_db_tenths(-120),
        );

        assert_eq!(lowered_spec.audio_tracks[0].channel, AudioChannel::A2);
        assert_eq!(lowered_spec.audio_tracks[0].mix.gain_db_tenths(), -120);
        assert!(!lowered_spec.audio_tracks[0].mix.is_muted());
    }

    #[test]
    fn wrap_usage_adds_layers_to_same_universal_spec() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(250)),
        );
        let mut spec =
            UniversalTimelineSpec::new(carrier).with_filmstrip(FilmstripUnderlay::Hidden);
        spec = spec.with_base_video(VideoLayerSourceSpec::VirtualShot(VirtualMediaRef::new(
            "tone",
            "clip_tone",
        )));
        spec.add_overlay(
            1,
            VideoLayerSourceSpec::VirtualShot(VirtualMediaRef::new("cover", "clip_cover")),
        );
        spec.add_marker("m1", MarkerKind::M, FrameNumber(100));

        assert_eq!(spec.overlays.len(), 1);
        assert_eq!(spec.markers[0].kind, MarkerKind::M);
        assert_eq!(spec.carrier.virtual_shot_id, "wrap");
    }
}
