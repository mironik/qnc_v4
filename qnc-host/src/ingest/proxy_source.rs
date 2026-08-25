//! Host adapter for shared TV source classification and proxy recipes.

use qnc_service_contracts::{FrameTimebase, MediaProbe as ContractMediaProbe, ScanMode};

use crate::ingest::thumb::MediaProbe;

pub use qnc_media_ffmpeg::proxy::{recipe_for_source, TvSourceClass};

pub fn classify_tv_source(probe: &MediaProbe) -> TvSourceClass {
    let Some(timebase) = source_timebase(probe.fps) else {
        return TvSourceClass::Other;
    };
    let (width, height) = parse_resolution(&probe.resolution);
    let contract_probe = ContractMediaProbe {
        width,
        height,
        duration_sec: Some(probe.duration_sec),
        timebase,
        scan_mode: scan_mode(probe),
        codec: probe.codec.clone(),
        field_order: probe.field_order.clone(),
        frame_count: None,
        duration_frames: None,
        has_video: width > 0 && height > 0,
        has_audio: probe.has_audio,
        audio_channels: probe.audio_channels as u16,
    };
    qnc_media_ffmpeg::proxy::classify_tv_source(&contract_probe)
}

fn source_timebase(fps: f64) -> Option<FrameTimebase> {
    let (fps_num, fps_den) = rational_fps(fps)?;
    FrameTimebase::new(fps_num, fps_den).ok()
}

fn rational_fps(fps: f64) -> Option<(u32, u32)> {
    if !fps.is_finite() || fps <= 0.0 {
        return None;
    }
    const NTSC: [(u32, u32); 4] = [(24000, 1001), (30000, 1001), (48000, 1001), (60000, 1001)];
    for (num, den) in NTSC {
        if (fps - (num as f64 / den as f64)).abs() < 0.01 {
            return Some((num, den));
        }
    }
    let rounded = fps.round();
    if (fps - rounded).abs() < 0.001 && rounded >= 1.0 {
        return Some((rounded as u32, 1));
    }
    Some(((fps * 1000.0).round() as u32, 1000))
}

fn parse_resolution(value: &str) -> (u32, u32) {
    let Some((width, height)) = value.trim().split_once('x') else {
        return (0, 0);
    };
    (
        width.trim().parse().unwrap_or(0),
        height.trim().parse().unwrap_or(0),
    )
}

fn scan_mode(probe: &MediaProbe) -> ScanMode {
    let order = probe.field_order.trim().to_ascii_lowercase();
    if !probe.interlaced || order == "progressive" {
        return ScanMode::Progressive;
    }
    match order.as_str() {
        "bb" | "bt" => ScanMode::InterlacedBottomFieldFirst,
        "tt" | "tb" => ScanMode::InterlacedTopFieldFirst,
        _ => ScanMode::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(fps: f64, interlaced: bool) -> MediaProbe {
        MediaProbe {
            duration_sec: 10.0,
            fps,
            fps_num: fps.round() as i64,
            fps_den: 1,
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
    fn classify_common_rates_without_progressive_pal_profile() {
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
            TvSourceClass::Other
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
            qnc_media_ffmpeg::proxy::ProxyScale::Native
        );
    }
}
