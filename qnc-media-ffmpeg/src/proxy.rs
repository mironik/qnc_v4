use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use qnc_service_contracts::{FrameTimebase, MediaProbe, ScanMode};

use crate::FfmpegToolchain;

const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "aif", "aiff", "flac", "caf", "w64", "bwav", "rf64", "mp3", "m4a", "aac",
];
const PROXY_MAXRATE: &str = "8M";
const PROXY_BUFSIZE: &str = "16M";
const PROXY_PRESET: &str = "ultrafast";
const PROXY_CRF: &str = "23";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfmpegProxyBuildOptions {
    pub toolchain: FfmpegToolchain,
    pub preferred_encoder: Option<ProxyVideoEncoder>,
    pub vaapi_device: Option<String>,
}

impl Default for FfmpegProxyBuildOptions {
    fn default() -> Self {
        Self {
            toolchain: FfmpegToolchain::default(),
            preferred_encoder: None,
            vaapi_device: None,
        }
    }
}

impl FfmpegProxyBuildOptions {
    pub fn with_preferred_encoder(mut self, encoder: Option<ProxyVideoEncoder>) -> Self {
        self.preferred_encoder = encoder;
        self
    }

    pub fn with_vaapi_device(mut self, device: Option<String>) -> Self {
        self.vaapi_device = device;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyVideoEncoder {
    Nvenc,
    Amf,
    Qsv,
    VideoToolbox,
    Vaapi,
    Libx264,
}

impl ProxyVideoEncoder {
    pub fn id(self) -> &'static str {
        match self {
            Self::Nvenc => "h264_nvenc",
            Self::Amf => "h264_amf",
            Self::Qsv => "h264_qsv",
            Self::VideoToolbox => "h264_videotoolbox",
            Self::Vaapi => "h264_vaapi",
            Self::Libx264 => "libx264",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Nvenc => "nvenc",
            Self::Amf => "amf",
            Self::Qsv => "qsv",
            Self::VideoToolbox => "videotoolbox",
            Self::Vaapi => "vaapi",
            Self::Libx264 => "libx264",
        }
    }

    pub fn uses_gpu(self) -> bool {
        !matches!(self, Self::Libx264)
    }

    pub fn from_label(label: &str) -> Self {
        parse_forced_encoder(label).unwrap_or(Self::Libx264)
    }
}

pub fn parse_forced_encoder(raw: &str) -> Option<ProxyVideoEncoder> {
    match raw.to_ascii_lowercase().as_str() {
        "" | "auto" => None,
        "nvenc" | "h264_nvenc" => Some(ProxyVideoEncoder::Nvenc),
        "amf" | "h264_amf" => Some(ProxyVideoEncoder::Amf),
        "qsv" | "h264_qsv" | "quicksync" => Some(ProxyVideoEncoder::Qsv),
        "videotoolbox" | "vt" | "h264_videotoolbox" => Some(ProxyVideoEncoder::VideoToolbox),
        "vaapi" | "h264_vaapi" => Some(ProxyVideoEncoder::Vaapi),
        "libx264" | "x264" | "cpu" | "software" => Some(ProxyVideoEncoder::Libx264),
        _ => None,
    }
}

pub fn platform_encoder_priority() -> &'static [ProxyVideoEncoder] {
    #[cfg(target_os = "macos")]
    {
        &[ProxyVideoEncoder::VideoToolbox, ProxyVideoEncoder::Libx264]
    }
    #[cfg(windows)]
    {
        &[
            ProxyVideoEncoder::Nvenc,
            ProxyVideoEncoder::Amf,
            ProxyVideoEncoder::Qsv,
            ProxyVideoEncoder::Libx264,
        ]
    }
    #[cfg(target_os = "linux")]
    {
        &[
            ProxyVideoEncoder::Nvenc,
            ProxyVideoEncoder::Vaapi,
            ProxyVideoEncoder::Qsv,
            ProxyVideoEncoder::Libx264,
        ]
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        &[ProxyVideoEncoder::Libx264]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TvRegion {
    Pal,
    Ntsc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TvSourceClass {
    Pal50p,
    Pal50i,
    Ntsc60p,
    Ntsc60i,
    Ntsc30p,
    Other,
}

impl TvSourceClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pal50p => "pal_50p",
            Self::Pal50i => "pal_50i",
            Self::Ntsc60p => "ntsc_60p",
            Self::Ntsc60i => "ntsc_60i",
            Self::Ntsc30p => "ntsc_30p",
            Self::Other => "other",
        }
    }

    pub fn region(self) -> Option<TvRegion> {
        match self {
            Self::Pal50p | Self::Pal50i => Some(TvRegion::Pal),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyCodec {
    H264,
    XdcamHd422,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyScale {
    Native,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProxyRecipe {
    pub codec: ProxyCodec,
    pub scale: ProxyScale,
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

pub fn recipe_for_source(class: TvSourceClass) -> ProxyRecipe {
    match class {
        TvSourceClass::Pal50p | TvSourceClass::Ntsc60p | TvSourceClass::Other => ProxyRecipe {
            codec: ProxyCodec::H264,
            scale: ProxyScale::Native,
            keep_interlace: false,
        },
        TvSourceClass::Pal50i | TvSourceClass::Ntsc60i => ProxyRecipe {
            codec: ProxyCodec::XdcamHd422,
            scale: ProxyScale::Native,
            keep_interlace: true,
        },
        TvSourceClass::Ntsc30p => ProxyRecipe {
            codec: ProxyCodec::XdcamHd422,
            scale: ProxyScale::Native,
            keep_interlace: false,
        },
    }
}

pub fn classify_tv_source(probe: &MediaProbe) -> TvSourceClass {
    let fps = fps_value(&probe.timebase);
    let interlaced = !matches!(probe.scan_mode, ScanMode::Progressive);

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
    if near(fps, 29.97, 0.6) || near(fps, 30.0, 0.6) {
        return TvSourceClass::Ntsc30p;
    }
    TvSourceClass::Other
}

pub fn active_proxy_encoder_label() -> String {
    let options = FfmpegProxyBuildOptions::default();
    resolve_proxy_encoder(&options).label().to_string()
}

pub fn proxy_path_for_recipe(proxy_dir: &Path, clip_id: &str, recipe: ProxyRecipe) -> PathBuf {
    proxy_dir.join(format!(
        "{}.{}",
        safe_clip_stem(clip_id),
        recipe.extension()
    ))
}

pub fn proxy_dest_for_source(
    proxy_dir: &Path,
    clip_id: &str,
    source: &Path,
) -> Result<PathBuf, String> {
    let options = FfmpegProxyBuildOptions::default();
    proxy_dest_for_source_with_options(proxy_dir, clip_id, source, &options)
}

pub fn proxy_dest_for_source_with_options(
    proxy_dir: &Path,
    clip_id: &str,
    source: &Path,
    options: &FfmpegProxyBuildOptions,
) -> Result<PathBuf, String> {
    let probe = probe_media_with_options(source, options)?;
    let class = classify_tv_source(&probe);
    let recipe = recipe_for_source(class);
    Ok(proxy_path_for_recipe(proxy_dir, clip_id, recipe))
}

pub fn generate_field_proxy(source: &Path, dest: &Path) -> Result<MediaProbe, String> {
    let options = FfmpegProxyBuildOptions::default();
    generate_field_proxy_with_options(source, dest, &options)
}

pub fn generate_field_proxy_with_options(
    source: &Path,
    dest: &Path,
    options: &FfmpegProxyBuildOptions,
) -> Result<MediaProbe, String> {
    if !source.is_file() {
        return Err(format!("izvor ne postoji: {}", source.display()));
    }
    if is_audio_media_file(source) {
        return Err(format!(
            "audio-only se ne enkodira kao video proxy (copy umjesto generate): {}",
            source.display()
        ));
    }
    let source_probe = probe_media_with_options(source, options)?;
    let class = classify_tv_source(&source_probe);
    let recipe = recipe_for_source(class);

    if dest.is_file() && dest.metadata().map(|m| m.len()).unwrap_or(0) > 0 {
        if verify_frame_parity_with_options(dest, &source_probe, options).is_ok() {
            return probe_media_with_options(dest, options);
        }
        let _ = std::fs::remove_file(dest);
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let started = Instant::now();
    match recipe.codec {
        ProxyCodec::XdcamHd422 => {
            run_xdcam_hd422_encode(source, dest, &source_probe, recipe, options)?;
        }
        ProxyCodec::H264 => {
            run_h264_encode(source, dest, &source_probe, recipe, options)?;
        }
    }
    verify_frame_parity_with_options(dest, &source_probe, options)?;
    let output_probe = probe_media_with_options(dest, options)?;
    let elapsed = started.elapsed().as_secs_f64().max(0.001);
    let duration = source_probe.duration_sec.unwrap_or(0.0).max(0.001);
    let _realtime_ratio = duration / elapsed;
    Ok(output_probe)
}

pub fn probe_media(source: &Path) -> Result<MediaProbe, String> {
    let options = FfmpegProxyBuildOptions::default();
    probe_media_with_options(source, &options)
}

pub fn probe_media_with_options(
    source: &Path,
    options: &FfmpegProxyBuildOptions,
) -> Result<MediaProbe, String> {
    if !source.is_file() {
        return Err(format!("izvor ne postoji: {}", source.display()));
    }
    let ffprobe = options.toolchain.ffprobe();
    let output = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,codec_name,width,height,avg_frame_rate,r_frame_rate,duration,field_order,channels,nb_frames",
            "-show_entries",
            "format=duration",
            "-of",
            "json",
        ])
        .arg(source)
        .output()
        .map_err(|e| format!("ffprobe pokretanje: {e}"))?;
    if !output.status.success() {
        return Err(stderr_or_default(&output, "ffprobe neuspjesan"));
    }
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("ffprobe json: {e}"))?;
    probe_from_json(&json)
}

fn probe_from_json(json: &serde_json::Value) -> Result<MediaProbe, String> {
    let streams = json
        .get("streams")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let video = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("video"));
    let audio = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("audio"));
    if video.is_none() && audio.is_none() {
        return Err("media nema video ni audio stream".into());
    }

    let duration_sec = json
        .get("format")
        .and_then(|f| f.get("duration"))
        .and_then(|v| v.as_str())
        .and_then(parse_decimal)
        .filter(|d| *d > 0.0)
        .or_else(|| {
            video
                .and_then(|s| s.get("duration"))
                .and_then(|v| v.as_str())
                .and_then(parse_decimal)
                .filter(|d| *d > 0.0)
        })
        .or_else(|| {
            audio
                .and_then(|s| s.get("duration"))
                .and_then(|v| v.as_str())
                .and_then(parse_decimal)
                .filter(|d| *d > 0.0)
        });

    let (fps_num, fps_den) = video
        .and_then(|s| s.get("avg_frame_rate"))
        .and_then(|v| v.as_str())
        .and_then(parse_frame_rate)
        .or_else(|| {
            video
                .and_then(|s| s.get("r_frame_rate"))
                .and_then(|v| v.as_str())
                .and_then(parse_frame_rate)
        })
        .unwrap_or((1, 1));
    let timebase = FrameTimebase::new(fps_num, fps_den).map_err(|e| e.message)?;
    let fps = fps_value(&timebase);
    let frame_count = video
        .and_then(|s| s.get("nb_frames"))
        .and_then(|v| v.as_str().and_then(|s| s.parse::<i64>().ok()))
        .filter(|v| *v > 0);
    let duration_frames = frame_count.or_else(|| {
        duration_sec
            .and_then(|duration| (fps > 0.0).then_some((duration.max(0.0) * fps).round() as i64))
    });
    let field_order = video
        .and_then(|s| s.get("field_order"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    Ok(MediaProbe {
        width: video
            .and_then(|s| s.get("width"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        height: video
            .and_then(|s| s.get("height"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        duration_sec,
        timebase,
        scan_mode: scan_mode(&field_order),
        codec: video
            .or(audio)
            .and_then(|s| s.get("codec_name"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string(),
        field_order,
        frame_count,
        duration_frames,
        has_video: video.is_some(),
        has_audio: audio.is_some(),
        audio_channels: streams
            .iter()
            .filter(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("audio"))
            .filter_map(|s| s.get("channels").and_then(|v| v.as_u64()))
            .max()
            .map(|channels| (channels as u16).clamp(1, 8))
            .unwrap_or(0),
    })
}

pub fn resolve_proxy_encoder(options: &FfmpegProxyBuildOptions) -> ProxyVideoEncoder {
    if let Some(encoder) = options.preferred_encoder {
        return encoder;
    }
    if let Ok(raw) = std::env::var("QNC_PROXY_ENCODER") {
        if let Some(encoder) = parse_forced_encoder(raw.trim()) {
            return encoder;
        }
    }
    if matches!(
        std::env::var("QNC_HW_ENCODE").as_deref(),
        Ok(v) if v == "0" || v.eq_ignore_ascii_case("false")
    ) {
        return ProxyVideoEncoder::Libx264;
    }
    let available = h264_encoders_from_ffmpeg(options.toolchain.ffmpeg());
    for &encoder in platform_encoder_priority() {
        if available.contains(encoder.id()) {
            return encoder;
        }
    }
    ProxyVideoEncoder::Libx264
}

pub fn append_proxy_video_encode_args(
    cmd: &mut Command,
    encoder: ProxyVideoEncoder,
    scale_filter: &str,
    fps_arg: &str,
) {
    if !scale_filter.is_empty() {
        cmd.args(["-vf", scale_filter]);
    }
    match encoder {
        ProxyVideoEncoder::Nvenc => {
            cmd.args([
                "-c:v",
                "h264_nvenc",
                "-preset",
                "p1",
                "-tune",
                "ll",
                "-rc",
                "vbr",
                "-cq",
                "28",
                "-bf",
                "0",
                "-maxrate",
                PROXY_MAXRATE,
                "-bufsize",
                PROXY_BUFSIZE,
            ]);
        }
        ProxyVideoEncoder::Amf => {
            cmd.args([
                "-c:v",
                "h264_amf",
                "-quality",
                "speed",
                "-rc",
                "vbr_latency",
                "-qv",
                "28",
                "-maxrate",
                PROXY_MAXRATE,
                "-bufsize",
                PROXY_BUFSIZE,
                "-pix_fmt",
                "yuv420p",
            ]);
        }
        ProxyVideoEncoder::Qsv => {
            cmd.args([
                "-c:v",
                "h264_qsv",
                "-preset",
                "veryfast",
                "-look_ahead",
                "0",
                "-bf",
                "0",
                "-async_depth",
                "4",
                "-global_quality",
                "28",
                "-maxrate",
                PROXY_MAXRATE,
                "-bufsize",
                PROXY_BUFSIZE,
            ]);
        }
        ProxyVideoEncoder::VideoToolbox => {
            cmd.args([
                "-c:v",
                "h264_videotoolbox",
                "-b:v",
                PROXY_MAXRATE,
                "-maxrate",
                PROXY_MAXRATE,
                "-bufsize",
                PROXY_BUFSIZE,
                "-pix_fmt",
                "yuv420p",
            ]);
        }
        ProxyVideoEncoder::Vaapi => {
            cmd.args([
                "-c:v",
                "h264_vaapi",
                "-qp",
                "28",
                "-maxrate",
                PROXY_MAXRATE,
                "-bufsize",
                PROXY_BUFSIZE,
            ]);
        }
        ProxyVideoEncoder::Libx264 => {
            cmd.args([
                "-c:v",
                "libx264",
                "-preset",
                PROXY_PRESET,
                "-tune",
                "fastdecode",
                "-crf",
                PROXY_CRF,
                "-bf",
                "0",
                "-threads",
                "0",
                "-maxrate",
                PROXY_MAXRATE,
                "-bufsize",
                PROXY_BUFSIZE,
                "-pix_fmt",
                "yuv420p",
            ]);
        }
    }
    cmd.args(["-r", fps_arg, "-fps_mode", "cfr"]);
}

pub fn append_decode_hwaccel_mode(
    cmd: &mut Command,
    encoder: ProxyVideoEncoder,
    keep_on_gpu: bool,
    vaapi_device: Option<&str>,
) {
    match encoder {
        ProxyVideoEncoder::Qsv => {
            cmd.args(["-hwaccel", "qsv"]);
            if keep_on_gpu {
                cmd.args(["-hwaccel_output_format", "qsv"]);
            }
        }
        ProxyVideoEncoder::Nvenc => {
            cmd.args(["-hwaccel", "cuda"]);
            if keep_on_gpu {
                cmd.args(["-hwaccel_output_format", "cuda"]);
            }
        }
        ProxyVideoEncoder::Amf => {
            cmd.args(["-hwaccel", "d3d11va"]);
        }
        ProxyVideoEncoder::Vaapi => {
            let device = vaapi_device
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    std::env::var("QNC_VAAPI_DEVICE")
                        .ok()
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty())
                })
                .unwrap_or_else(|| "/dev/dri/renderD128".to_string());
            cmd.args(["-hwaccel", "vaapi", "-hwaccel_device", &device]);
            if keep_on_gpu {
                cmd.args(["-hwaccel_output_format", "vaapi"]);
            }
        }
        ProxyVideoEncoder::VideoToolbox => {
            cmd.args(["-hwaccel", "videotoolbox"]);
        }
        ProxyVideoEncoder::Libx264 => {}
    }
}

fn run_h264_encode(
    source: &Path,
    dest: &Path,
    probe: &MediaProbe,
    recipe: ProxyRecipe,
    options: &FfmpegProxyBuildOptions,
) -> Result<(), String> {
    let encoder = resolve_proxy_encoder(options);
    if encoder == ProxyVideoEncoder::Libx264 {
        return run_h264_libx264(source, dest, probe, recipe, options);
    }
    match run_h264_hw(source, dest, probe, encoder, recipe, options) {
        Ok(()) => Ok(()),
        Err(_) => {
            let _ = std::fs::remove_file(dest);
            run_h264_libx264(source, dest, probe, recipe, options)
        }
    }
}

fn run_h264_libx264(
    source: &Path,
    dest: &Path,
    probe: &MediaProbe,
    _recipe: ProxyRecipe,
    options: &FfmpegProxyBuildOptions,
) -> Result<(), String> {
    run_encode_inner(
        source,
        dest,
        probe,
        EncodeAccel::Software,
        ProxyVideoEncoder::Libx264,
        "",
        options,
    )
}

fn run_h264_hw(
    source: &Path,
    dest: &Path,
    probe: &MediaProbe,
    encoder: ProxyVideoEncoder,
    _recipe: ProxyRecipe,
    options: &FfmpegProxyBuildOptions,
) -> Result<(), String> {
    run_encode_inner(
        source,
        dest,
        probe,
        EncodeAccel::GpuKeep,
        encoder,
        "",
        options,
    )
}

fn run_xdcam_hd422_encode(
    source: &Path,
    dest: &Path,
    probe: &MediaProbe,
    recipe: ProxyRecipe,
    options: &FfmpegProxyBuildOptions,
) -> Result<(), String> {
    require_fps(fps_value(&probe.timebase), "xdcam proxy source fps")?;
    let fps_arg = fps_arg(&probe.timebase);
    let gop = if fps_value(&probe.timebase) < 28.0 {
        "12"
    } else {
        "15"
    };

    let mut cmd = Command::new(options.toolchain.ffmpeg());
    cmd.args(["-y", "-hide_banner", "-nostdin", "-v", "error"]);
    cmd.args(["-i"]).arg(source);
    cmd.args(["-map", "0:v:0", "-map", "0:a:0?"]);

    let needs_xdcam_raster = !(probe.width == 1920 && probe.height == 1080);
    if needs_xdcam_raster {
        cmd.args([
            "-vf",
            "scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2",
        ]);
    }

    cmd.args([
        "-c:v",
        "mpeg2video",
        "-pix_fmt",
        "yuv422p",
        "-b:v",
        "50M",
        "-minrate",
        "50M",
        "-maxrate",
        "50M",
        "-bufsize",
        "50M",
        "-g",
        gop,
        "-bf",
        "2",
        "-mpv_flags",
        "+strict_gop",
        "-intra_vlc",
        "1",
        "-non_linear_quant",
        "1",
        "-qmin",
        "1",
        "-qmax",
        "12",
        "-dc",
        "10",
    ]);
    if recipe.keep_interlace {
        cmd.args(["-flags", "+ildct+ilme", "-top", "1"]);
    }
    cmd.args(["-r", &fps_arg, "-fps_mode", "cfr"]);
    cmd.args(["-c:a", "pcm_s16le", "-ar", "48000", "-ac", "2"]);
    cmd.args(["-f", "mxf"]);
    cmd.arg(dest);
    run_ffmpeg(cmd, dest, "ffmpeg xdcam proxy")
}

#[derive(Clone, Copy)]
enum EncodeAccel {
    GpuKeep,
    Software,
}

fn run_encode_inner(
    source: &Path,
    dest: &Path,
    probe: &MediaProbe,
    accel: EncodeAccel,
    encoder: ProxyVideoEncoder,
    scale_filter: &str,
    options: &FfmpegProxyBuildOptions,
) -> Result<(), String> {
    require_fps(fps_value(&probe.timebase), "h264 proxy source fps")?;
    let fps_arg = fps_arg(&probe.timebase);
    let mut cmd = Command::new(options.toolchain.ffmpeg());
    cmd.args(["-y", "-hide_banner", "-nostdin", "-v", "error"]);
    match accel {
        EncodeAccel::GpuKeep => {
            append_decode_hwaccel_mode(&mut cmd, encoder, true, options.vaapi_device.as_deref())
        }
        EncodeAccel::Software => {}
    }
    cmd.args(["-i"]).arg(source);
    cmd.args(["-map", "0:v:0", "-map", "0:a:0?"]);
    append_proxy_video_encode_args(&mut cmd, encoder, scale_filter, &fps_arg);
    cmd.args(["-c:a", "aac", "-b:a", "96k", "-ac", "2"]);
    cmd.arg(dest);
    run_ffmpeg(cmd, dest, "ffmpeg proxy")
}

fn run_ffmpeg(mut cmd: Command, dest: &Path, label: &str) -> Result<(), String> {
    let output = cmd.output().map_err(|e| format!("{label}: {e}"))?;
    if !output.status.success() {
        return Err(stderr_or_default(&output, label));
    }
    if !dest.is_file() {
        return Err("proxy datoteka nije kreirana".into());
    }
    Ok(())
}

fn verify_frame_parity_with_options(
    dest: &Path,
    source_probe: &MediaProbe,
    options: &FfmpegProxyBuildOptions,
) -> Result<(), String> {
    let source_fps = require_fps(fps_value(&source_probe.timebase), "proxy source fps")?;
    let dest_probe = probe_media_with_options(dest, options)?;
    let dest_fps = require_fps(fps_value(&dest_probe.timebase), "proxy output fps")?;
    if (source_fps - dest_fps).abs() > 0.08 {
        return Err(format!(
            "proxy fps {} != izvor {} - timecode bi se razmaknuo",
            dest_fps, source_fps
        ));
    }
    let Some(source_duration) = source_probe.duration_sec else {
        return Ok(());
    };
    let Some(dest_duration) = dest_probe.duration_sec else {
        return Ok(());
    };
    let src_frames = seconds_to_frame(source_duration, source_fps);
    let dst_frames = seconds_to_frame(dest_duration, dest_fps);
    let slack = 2i64;
    if (src_frames - dst_frames).abs() > slack {
        return Err(format!(
            "frame mismatch: izvor={src_frames} proxy={dst_frames} (max +/-{slack})"
        ));
    }
    Ok(())
}

fn h264_encoders_from_ffmpeg(ffmpeg: &Path) -> HashSet<String> {
    let output = Command::new(ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .output();
    let Ok(output) = output else {
        return HashSet::new();
    };
    parse_encoder_list(&String::from_utf8_lossy(&output.stdout))
}

fn parse_encoder_list(text: &str) -> HashSet<String> {
    let ids = [
        "h264_nvenc",
        "h264_amf",
        "h264_qsv",
        "h264_videotoolbox",
        "h264_vaapi",
        "libx264",
    ];
    let mut set = HashSet::new();
    for line in text.lines() {
        for id in ids {
            if line.contains(id) {
                set.insert(id.to_string());
            }
        }
    }
    set
}

fn is_audio_media_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn safe_clip_stem(clip_id: &str) -> String {
    let safe: String = clip_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() { "clip".into() } else { safe }
}

fn scan_mode(field_order: &str) -> ScanMode {
    match field_order.trim().to_ascii_lowercase().as_str() {
        "progressive" => ScanMode::Progressive,
        "tt" | "tb" => ScanMode::InterlacedTopFieldFirst,
        "bb" | "bt" => ScanMode::InterlacedBottomFieldFirst,
        _ => ScanMode::Unknown,
    }
}

fn parse_decimal(raw: &str) -> Option<f64> {
    let compact: String = raw
        .trim()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{00a0}')
        .collect();
    if compact.is_empty() {
        return None;
    }
    let split = compact
        .char_indices()
        .find(|(_, c)| *c == 'e' || *c == 'E')
        .map(|(index, _)| index);
    let (mantissa, exponent) = match split {
        Some(index) => compact.split_at(index),
        None => (compact.as_str(), ""),
    };
    let decimal_index = mantissa
        .char_indices()
        .filter_map(|(index, c)| (c == '.' || c == ',').then_some(index))
        .last();
    let mut out = String::with_capacity(compact.len());
    for (index, c) in mantissa.char_indices() {
        match c {
            '.' | ',' if Some(index) == decimal_index => out.push('.'),
            '.' | ',' | '\'' | '_' => {}
            _ => out.push(c),
        }
    }
    out.push_str(exponent);
    out.parse::<f64>().ok().filter(|value| value.is_finite())
}

fn parse_frame_rate(raw: &str) -> Option<(u32, u32)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "0/0" || trimmed.eq_ignore_ascii_case("n/a") {
        return None;
    }
    if let Some((num, den)) = trimmed.split_once('/') {
        let num = num.trim().parse::<u32>().ok()?;
        let den = den.trim().parse::<u32>().ok()?;
        if num > 0 && den > 0 {
            return Some((num, den));
        }
        return None;
    }
    let fps = parse_decimal(trimmed)?;
    rational_fps(fps)
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

fn fps_value(timebase: &FrameTimebase) -> f64 {
    if timebase.fps_den == 0 {
        0.0
    } else {
        timebase.fps_num as f64 / timebase.fps_den as f64
    }
}

fn fps_arg(timebase: &FrameTimebase) -> String {
    if timebase.fps_den == 1 {
        timebase.fps_num.to_string()
    } else {
        format!("{}/{}", timebase.fps_num, timebase.fps_den)
    }
}

fn require_fps(raw: f64, context: &str) -> Result<f64, String> {
    if raw.is_finite() && raw > 0.0 {
        Ok(raw)
    } else {
        Err(format!("{context}: missing valid FPS"))
    }
}

fn seconds_to_frame(seconds: f64, fps: f64) -> i64 {
    if !fps.is_finite() || fps <= 0.0 {
        return 0;
    }
    (seconds.max(0.0) * fps).round() as i64
}

fn near(value: f64, target: f64, tol: f64) -> bool {
    (value - target).abs() <= tol
}

fn stderr_or_default(output: &std::process::Output, fallback: &str) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.trim().is_empty() {
        fallback.to_string()
    } else {
        stderr.trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(fps_num: u32, fps_den: u32, scan_mode: ScanMode) -> MediaProbe {
        MediaProbe {
            width: 1920,
            height: 1080,
            duration_sec: Some(10.0),
            timebase: FrameTimebase { fps_num, fps_den },
            scan_mode,
            codec: "h264".into(),
            field_order: "progressive".into(),
            frame_count: Some(500),
            duration_frames: Some(500),
            has_video: true,
            has_audio: true,
            audio_channels: 2,
        }
    }

    #[test]
    fn classify_common_rates_without_progressive_pal_profile() {
        assert_eq!(
            classify_tv_source(&probe(50, 1, ScanMode::Progressive)),
            TvSourceClass::Pal50p
        );
        assert_eq!(
            classify_tv_source(&probe(25, 1, ScanMode::InterlacedTopFieldFirst)),
            TvSourceClass::Pal50i
        );
        assert_eq!(
            classify_tv_source(&probe(25, 1, ScanMode::Progressive)),
            TvSourceClass::Other
        );
        assert_eq!(
            classify_tv_source(&probe(60000, 1001, ScanMode::Progressive)),
            TvSourceClass::Ntsc60p
        );
    }

    #[test]
    fn recipes_keep_progressive_50p_h264_native() {
        assert_eq!(recipe_for_source(TvSourceClass::Pal50p).id(), "h264_native");
        assert_eq!(
            recipe_for_source(TvSourceClass::Pal50i).id(),
            "xdcam_hd422_i"
        );
    }

    #[test]
    fn proxy_path_uses_recipe_extension() {
        let dir = Path::new("/tmp/proxy");
        let recipe = recipe_for_source(TvSourceClass::Pal50i);
        assert_eq!(
            proxy_path_for_recipe(dir, "Clip 001", recipe)
                .file_name()
                .unwrap(),
            "Clip_001.mxf"
        );
    }

    #[test]
    fn parse_decimal_accepts_comma_and_dot() {
        assert_eq!(parse_decimal("29,97"), Some(29.97));
        assert_eq!(parse_decimal("1.234,56"), Some(1234.56));
        assert_eq!(parse_decimal("1,234.56"), Some(1234.56));
    }

    #[test]
    fn parse_encoder_list_finds_nvenc_and_x264() {
        let text = " V..... h264_nvenc NVIDIA NVENC H.264 encoder\n V..... libx264 libx264 H.264";
        let set = parse_encoder_list(text);
        assert!(set.contains("h264_nvenc"));
        assert!(set.contains("libx264"));
    }
}
