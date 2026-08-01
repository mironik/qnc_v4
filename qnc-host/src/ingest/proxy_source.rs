//! Klasifikacija TV izvora (PAL / NTSC) i recept za terenski proxy.
//!
//! Playback cilj = native Rust player. Interlace / 25p·30p → XDCAM HD422 MXF
//! (isti profil kao `video.timeline_codec = xdcam_hd_422` / `mpeg2_422_50mbit`).
//! H.264 editorial proxy zadržava **raster izvora** (ffprobe) — bez hardcodirane 720/1080 skale.

use crate::ingest::thumb::MediaProbe;

/// TV regija po frame-rate obitelji.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TvRegion {
    /// 25 / 50
    Pal,
    /// 29.97 / 30 / 59.94 / 60
    Ntsc,
}

/// Tipični broadcast / XAVC načini snimanja.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TvSourceClass {
    /// PAL 50 progressive (npr. XAVC 1080p50)
    Pal50p,
    /// PAL 50 interlaced (25 fps, 50 polja)
    Pal50i,
    /// PAL 25 progressive
    Pal25p,
    /// NTSC 59.94/60 progressive
    Ntsc60p,
    /// NTSC 59.94/60 interlaced (≈29.97 fps, 60 polja)
    Ntsc60i,
    /// NTSC 29.97/30 progressive
    Ntsc30p,
    /// Nepoznato — H.264 native raster
    Other,
}

impl TvSourceClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pal50p => "pal_50p",
            Self::Pal50i => "pal_50i",
            Self::Pal25p => "pal_25p",
            Self::Ntsc60p => "ntsc_60p",
            Self::Ntsc60i => "ntsc_60i",
            Self::Ntsc30p => "ntsc_30p",
            Self::Other => "other",
        }
    }

    pub fn region(self) -> Option<TvRegion> {
        match self {
            Self::Pal50p | Self::Pal50i | Self::Pal25p => Some(TvRegion::Pal),
            Self::Ntsc60p | Self::Ntsc60i | Self::Ntsc30p => Some(TvRegion::Ntsc),
            Self::Other => None,
        }
    }

    pub fn region_label(self) -> &'static str {
        match self.region() {
            Some(TvRegion::Pal) => "PAL",
            Some(TvRegion::Ntsc) => "NTSC",
            None => "other",
        }
    }
}

/// Recept encodea.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyCodec {
    /// Editorial H.264 — raster = ffprobe izvora (bez forced downscale).
    H264,
    /// Sony XDCAM HD422 — MPEG-2 4:2:2 50 Mbit u MXF OP1a.
    XdcamHd422,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyScale {
    /// Zadrži width×height iz ffprobea (H.264 editorial).
    Native,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProxyRecipe {
    pub codec: ProxyCodec,
    pub scale: ProxyScale,
    /// Zadrži interlace (50i/60i XDCAM).
    pub keep_interlace: bool,
}

impl ProxyRecipe {
    pub fn id(self) -> &'static str {
        match (self.codec, self.keep_interlace) {
            (ProxyCodec::H264, _) => "h264_native",
            (ProxyCodec::XdcamHd422, true) => "xdcam_hd422_i",
            (ProxyCodec::XdcamHd422, false) => "xdcam_hd422_p",
        }
    }

    pub fn extension(self) -> &'static str {
        match self.codec {
            ProxyCodec::H264 => "mp4",
            ProxyCodec::XdcamHd422 => "mxf",
        }
    }
}

/// Mapiranje izvora → proxy recept.
pub fn recipe_for_source(class: TvSourceClass) -> ProxyRecipe {
    match class {
        // Progressive H.264: codec change only — resolution from source probe.
        TvSourceClass::Pal50p | TvSourceClass::Ntsc60p | TvSourceClass::Other => ProxyRecipe {
            codec: ProxyCodec::H264,
            scale: ProxyScale::Native,
            keep_interlace: false,
        },
        // Interlace → XDCAM HD422, zadrži polja (profil traži 1920 kad treba).
        TvSourceClass::Pal50i | TvSourceClass::Ntsc60i => ProxyRecipe {
            codec: ProxyCodec::XdcamHd422,
            scale: ProxyScale::Native,
            keep_interlace: true,
        },
        // 25p / 30p → XDCAM HD422 progressive.
        TvSourceClass::Pal25p | TvSourceClass::Ntsc30p => ProxyRecipe {
            codec: ProxyCodec::XdcamHd422,
            scale: ProxyScale::Native,
            keep_interlace: false,
        },
    }
}

pub fn classify_tv_source(probe: &MediaProbe) -> TvSourceClass {
    let fps = probe.fps;
    let interlaced = probe.interlaced;

    if interlaced {
        if near(fps, 25.0, 0.6) || near(fps, 50.0, 1.0) {
            return TvSourceClass::Pal50i;
        }
        if near(fps, 29.97, 0.6)
            || near(fps, 30.0, 0.6)
            || near(fps, 59.94, 1.0)
            || near(fps, 60.0, 1.0)
        {
            return TvSourceClass::Ntsc60i;
        }
        if fps < 40.0 {
            return TvSourceClass::Pal50i;
        }
        return TvSourceClass::Ntsc60i;
    }

    if near(fps, 50.0, 1.0) {
        return TvSourceClass::Pal50p;
    }
    if near(fps, 59.94, 1.0) || near(fps, 60.0, 1.0) {
        return TvSourceClass::Ntsc60p;
    }
    if near(fps, 25.0, 0.6) || near(fps, 24.0, 0.6) || near(fps, 23.976, 0.6) {
        return TvSourceClass::Pal25p;
    }
    if near(fps, 29.97, 0.6) || near(fps, 30.0, 0.6) {
        return TvSourceClass::Ntsc30p;
    }
    TvSourceClass::Other
}

fn near(value: f64, target: f64, tol: f64) -> bool {
    (value - target).abs() <= tol
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(fps: f64, interlaced: bool) -> MediaProbe {
        MediaProbe {
            duration_sec: 10.0,
            fps,
            resolution: "1920x1080".into(),
            codec: "h264".into(),
            has_audio: true,
            audio_channels: 2,
            field_order: if interlaced {
                "tt".into()
            } else {
                "progressive".into()
            },
            interlaced,
        }
    }

    #[test]
    fn classify_common_rates() {
        assert_eq!(
            classify_tv_source(&probe(50.0, false)),
            TvSourceClass::Pal50p
        );
        assert_eq!(
            classify_tv_source(&probe(25.0, true)),
            TvSourceClass::Pal50i
        );
        assert_eq!(
            classify_tv_source(&probe(25.0, false)),
            TvSourceClass::Pal25p
        );
        assert_eq!(
            classify_tv_source(&probe(59.94, false)),
            TvSourceClass::Ntsc60p
        );
        assert_eq!(
            classify_tv_source(&probe(29.97, true)),
            TvSourceClass::Ntsc60i
        );
        assert_eq!(
            classify_tv_source(&probe(29.97, false)),
            TvSourceClass::Ntsc30p
        );
    }

    #[test]
    fn h264_recipe_is_native_raster() {
        assert_eq!(recipe_for_source(TvSourceClass::Pal50p).id(), "h264_native");
        assert_eq!(
            recipe_for_source(TvSourceClass::Other).scale,
            ProxyScale::Native
        );
    }
}
