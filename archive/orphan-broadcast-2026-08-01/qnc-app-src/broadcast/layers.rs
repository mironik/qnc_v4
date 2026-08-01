//! QNC/Kodak program layer taxonomy.
//!
//! Program layers are registered against the transparent celluloid/timecode
//! carrier. Filmstrip is a visual underlay below that carrier, not a playback
//! source. Base video, overlays, audio channels, and effects are program layers.

use super::timebase::{FrameNumber, FrameRange};

pub const MAX_AUDIO_CHANNELS: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ZPriority(i32);

impl ZPriority {
    pub const FILMSTRIP_UNDERLAY: Self = Self(-1_000);
    pub const BASE_VIDEO: Self = Self(0);
    pub const OVERLAY_BASE: Self = Self(1_000);
    pub const MARKER: Self = Self(9_000);
    pub const EFFECT: Self = Self(10_000);

    pub fn new(value: i32) -> Self {
        Self(value)
    }

    pub fn value(self) -> i32 {
        self.0
    }

    pub fn above(self, other: Self) -> bool {
        self > other
    }

    pub fn below(self, other: Self) -> bool {
        self < other
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AudioChannel(u8);

impl AudioChannel {
    pub const A1: Self = Self(1);
    pub const A2: Self = Self(2);
    pub const A3: Self = Self(3);
    pub const A4: Self = Self(4);

    pub fn new(channel: u8) -> Option<Self> {
        if (1..=MAX_AUDIO_CHANNELS).contains(&channel) {
            Some(Self(channel))
        } else {
            None
        }
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioMix {
    gain_db_tenths: i16,
    muted: bool,
}

impl AudioMix {
    pub const MIN_GAIN_DB_TENTHS: i16 = -960;
    pub const MAX_GAIN_DB_TENTHS: i16 = 120;
    pub const UNITY: Self = Self {
        gain_db_tenths: 0,
        muted: false,
    };
    pub const MUTED: Self = Self {
        gain_db_tenths: 0,
        muted: true,
    };

    pub fn new(gain_db_tenths: i16, muted: bool) -> Self {
        Self {
            gain_db_tenths: gain_db_tenths
                .clamp(Self::MIN_GAIN_DB_TENTHS, Self::MAX_GAIN_DB_TENTHS),
            muted,
        }
    }

    pub fn gain_db_tenths(self) -> i16 {
        self.gain_db_tenths
    }

    pub fn is_muted(self) -> bool {
        self.muted
    }

    pub fn is_audible(self) -> bool {
        !self.muted
    }

    pub fn effective_linear_gain(self) -> f32 {
        if self.muted {
            0.0
        } else {
            10.0_f32.powf(self.gain_db_tenths as f32 / 200.0)
        }
    }

    pub fn muted() -> Self {
        Self::MUTED
    }

    pub fn with_gain_db_tenths(gain_db_tenths: i16) -> Self {
        Self::new(gain_db_tenths, false)
    }
}

impl Default for AudioMix {
    fn default() -> Self {
        Self::UNITY
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    Marker,
    Cut,
    Dissolve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerKind {
    In,
    Out,
    M,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramLayerKind {
    /// Thumbnail/editorial underlay below transparent carrier.
    /// Never decode this for playback.
    Filmstrip,
    /// Main tone/interview/program video.
    BaseVideo,
    /// Program audio channel 1–4.
    Audio(AudioChannel),
    /// One or more cover/pokrivalice layers above base video.
    Overlay { index: u8 },
    /// IN/OUT/M marker layer attached to the carrier.
    Marker(MarkerKind),
    /// Marker/effect layer: M marker = cut effect; later dissolve etc.
    Effect(EffectKind),
}

impl ProgramLayerKind {
    pub fn visual_z_priority(self) -> Option<ZPriority> {
        match self {
            Self::Filmstrip => Some(ZPriority::FILMSTRIP_UNDERLAY),
            Self::BaseVideo => Some(ZPriority::BASE_VIDEO),
            Self::Audio(_) => None,
            Self::Overlay { index } => Some(ZPriority::new(
                ZPriority::OVERLAY_BASE.value() + index as i32,
            )),
            Self::Marker(_) => Some(ZPriority::MARKER),
            Self::Effect(_) => Some(ZPriority::EFFECT),
        }
    }

    pub fn is_playback_media(self) -> bool {
        !matches!(self, Self::Filmstrip | Self::Marker(_) | Self::Effect(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramLayerSource {
    /// Uses only the carrier/timecode; not media.
    TimecodeCarrier,
    /// UI thumbnails only. Must not be used as video playback input.
    FilmstripPreview { virtual_shot_id: String },
    /// Decoded video from a DB-backed virtual shot.
    VirtualShotVideo {
        virtual_shot_id: String,
        clip_id: String,
    },
    /// Decoded audio from a DB-backed virtual shot.
    VirtualShotAudio {
        virtual_shot_id: String,
        clip_id: String,
        channel: AudioChannel,
    },
    /// Explicit video black for missing/off video.
    BlankVideo,
    /// Explicit silence for missing audio or empty channels.
    Silence { channel: AudioChannel },
    /// IN/OUT/M marker attached to carrier timecode.
    MarkerEvent {
        kind: MarkerKind,
        marker_id: String,
        frame: FrameNumber,
    },
    /// Marker/cut/dissolve event attached to carrier timecode.
    EffectEvent {
        effect: EffectKind,
        marker_id: String,
        frame: FrameNumber,
    },
}

impl ProgramLayerSource {
    pub fn is_filmstrip_preview(&self) -> bool {
        matches!(self, Self::FilmstripPreview { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramLayer {
    pub id: String,
    pub kind: ProgramLayerKind,
    pub source: ProgramLayerSource,
    pub frame_range: Option<FrameRange>,
    pub audio_mix: Option<AudioMix>,
    pub enabled: bool,
}

impl ProgramLayer {
    pub fn new(id: impl Into<String>, kind: ProgramLayerKind, source: ProgramLayerSource) -> Self {
        Self {
            id: id.into(),
            kind,
            source,
            frame_range: None,
            audio_mix: None,
            enabled: true,
        }
    }

    pub fn with_frame_range(mut self, frame_range: FrameRange) -> Self {
        self.frame_range = Some(frame_range);
        self
    }

    pub fn with_audio_mix(mut self, audio_mix: AudioMix) -> Self {
        self.audio_mix = Some(audio_mix);
        self
    }

    pub fn is_active_at(&self, frame: FrameNumber) -> bool {
        self.enabled
            && self
                .frame_range
                .map(|range| range.contains(frame))
                .unwrap_or(true)
    }

    pub fn is_playback_media(&self) -> bool {
        self.enabled && self.kind.is_playback_media() && !self.source.is_filmstrip_preview()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_channels_are_limited_to_broadcast_project_range() {
        assert_eq!(AudioChannel::new(1).unwrap().get(), 1);
        assert_eq!(AudioChannel::new(4).unwrap().get(), 4);
        assert!(AudioChannel::new(0).is_none());
        assert!(AudioChannel::new(5).is_none());
    }

    #[test]
    fn audio_mix_supports_per_layer_mute_and_gain() {
        assert_eq!(AudioMix::default(), AudioMix::UNITY);
        assert!(!AudioMix::with_gain_db_tenths(-120).is_muted());
        assert_eq!(AudioMix::with_gain_db_tenths(-120).gain_db_tenths(), -120);
        assert!(AudioMix::muted().is_muted());
        assert!(!AudioMix::muted().is_audible());
        assert_eq!(AudioMix::muted().effective_linear_gain(), 0.0);
        assert_eq!(
            AudioMix::with_gain_db_tenths(-1_200).gain_db_tenths(),
            AudioMix::MIN_GAIN_DB_TENTHS
        );
    }

    #[test]
    fn filmstrip_layer_is_not_playback_media() {
        let layer = ProgramLayer::new(
            "filmstrip",
            ProgramLayerKind::Filmstrip,
            ProgramLayerSource::FilmstripPreview {
                virtual_shot_id: "shot".into(),
            },
        );

        assert!(!layer.is_playback_media());
    }

    #[test]
    fn filmstrip_underlay_sorts_below_program_video() {
        assert!(ProgramLayerKind::Filmstrip
            .visual_z_priority()
            .unwrap()
            .below(ProgramLayerKind::BaseVideo.visual_z_priority().unwrap()));
        assert!(ProgramLayerKind::Filmstrip
            .visual_z_priority()
            .unwrap()
            .below(
                ProgramLayerKind::Overlay { index: 1 }
                    .visual_z_priority()
                    .unwrap()
            ));
    }

    #[test]
    fn overlay_layers_sort_above_base_video() {
        assert!(ProgramLayerKind::Overlay { index: 1 }
            .visual_z_priority()
            .unwrap()
            .above(ProgramLayerKind::BaseVideo.visual_z_priority().unwrap()));
        assert!(ProgramLayerKind::Effect(EffectKind::Marker)
            .visual_z_priority()
            .unwrap()
            .above(
                ProgramLayerKind::Overlay { index: 1 }
                    .visual_z_priority()
                    .unwrap()
            ));
    }

    #[test]
    fn audio_layer_is_not_part_of_visual_z_axis() {
        assert_eq!(
            ProgramLayerKind::Audio(AudioChannel::new(1).unwrap()).visual_z_priority(),
            None
        );
    }

    #[test]
    fn marker_layer_is_not_playback_media() {
        let layer = ProgramLayer::new(
            "marker:in",
            ProgramLayerKind::Marker(MarkerKind::In),
            ProgramLayerSource::MarkerEvent {
                kind: MarkerKind::In,
                marker_id: "in".into(),
                frame: FrameNumber(0),
            },
        );

        assert!(!layer.is_playback_media());
    }

    #[test]
    fn layer_range_controls_activity_on_carrier() {
        let layer = ProgramLayer::new(
            "overlay:1",
            ProgramLayerKind::Overlay { index: 1 },
            ProgramLayerSource::BlankVideo,
        )
        .with_frame_range(FrameRange::new(FrameNumber(10), FrameNumber(20)));

        assert!(!layer.is_active_at(FrameNumber(9)));
        assert!(layer.is_active_at(FrameNumber(10)));
        assert!(!layer.is_active_at(FrameNumber(20)));
    }
}
