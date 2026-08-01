/// H.264 enkoder za terenski proxy — dijeljen između hardware profila i ffmpeg arg builder-a.
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
        match label.trim().to_ascii_lowercase().as_str() {
            "nvenc" | "h264_nvenc" => Self::Nvenc,
            "amf" | "h264_amf" => Self::Amf,
            "qsv" | "h264_qsv" => Self::Qsv,
            "videotoolbox" | "vt" | "h264_videotoolbox" => Self::VideoToolbox,
            "vaapi" | "h264_vaapi" => Self::Vaapi,
            _ => Self::Libx264,
        }
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
