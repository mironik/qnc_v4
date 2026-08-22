use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const INPUT_FORMATS: &[(&str, &str)] = &[
    ("HD 1080p50", "HD 1080p50 (PAL)"),
    ("HD 1080i50", "HD 1080i50 (PAL)"),
    ("HD 1080p30", "HD 1080p29.97 (NTSC)"),
    ("HD 1080p60", "HD 1080p59.94 (NTSC)"),
    ("HD 1080i60", "HD 1080i59.94 (NTSC)"),
    ("UHD 2160p", "UHD 2160p"),
];

pub const FPS_OPTIONS: &[&str] = &["25", "50", "29.97", "30", "59.94", "60"];

pub const FIELD_ORDER: &[(&str, &str)] = &[
    ("progressive", "Progressive"),
    ("upper_first", "Upper first (i)"),
];

pub const COLOR_SPACE: &[(&str, &str)] = &[("rec709", "rec709"), ("rec2020", "rec2020")];

pub const CONTAINERS: &[(&str, &str)] = &[("mxf_op1a", "MXF OP1a"), ("mp4", "MP4"), ("mov", "MOV")];

pub const VIDEO_CODECS: &[(&str, &str)] = &[
    ("mpeg2_422_50mbit", "MPEG-2 422 50 Mbit"),
    ("h264", "H.264"),
    ("prores_422", "ProRes 422"),
    ("dnxhd_hq", "DNxHD HQ"),
];

pub const INGEST_PROFILES: &[(&str, &str)] = &[("field", "Teren"), ("house", "TV kuca")];

pub const PROXY_POLICIES: &[(&str, &str)] = &[
    ("generate_if_missing", "Generiraj ako nema"),
    ("copy_to_project", "Kopiraj u projekt"),
    ("use_house_media", "Kucni medij"),
    ("link_when_available", "Link"),
];

pub const EXPORT_MODES: &[(&str, &str)] = &[
    ("xml_master", "XML master"),
    ("xdcam", "XDCAM"),
    ("original", "Original"),
    ("avid", "Avid"),
];

pub const ORIGINAL_POLICIES: &[(&str, &str)] = &[
    ("link_when_available", "Link"),
    ("copy_background", "Kopiraj u pozadini"),
    ("ignore_for_fast_news", "Ignoriraj (brze vijesti)"),
];

pub const AUDIO_RATES: &[&str] = &["48000", "44100"];
pub const AUDIO_CHANNELS: &[&str] = &["2", "4", "6", "8"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportProfileCatalog {
    pub catalog_id: String,
    #[serde(default)]
    pub presets: Vec<ExportProfilePreset>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportProfilePreset {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub values: Value,
}

pub fn bundled_export_profile_catalog() -> ExportProfileCatalog {
    serde_json::from_str(include_str!("../export_profiles.json"))
        .expect("bundled export_profiles.json must be valid")
}

pub fn builtin_export_presets() -> Vec<ExportProfilePreset> {
    bundled_export_profile_catalog().presets
}

pub fn export_profile_values(
    format: &str,
    fps: f64,
    width: i64,
    height: i64,
    field_order: &str,
    color_space: &str,
    container: &str,
    video_codec: &str,
    audio_sample_rate: i64,
    audio_channels: i64,
) -> Value {
    json!({
        "format": format,
        "fps": fps,
        "width": width,
        "height": height,
        "field_order": field_order,
        "color_space": color_space,
        "container": container,
        "video_codec": video_codec,
        "audio_sample_rate": audio_sample_rate,
        "audio_channels": audio_channels,
    })
}

fn value_str<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn parse_decimal(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.replace(',', ".").parse::<f64>().ok()
}

fn value_fps(value: &Value) -> f64 {
    value
        .get("fps")
        .and_then(Value::as_f64)
        .or_else(|| value.get("fps").and_then(Value::as_i64).map(|v| v as f64))
        .or_else(|| {
            value
                .get("fps")
                .and_then(Value::as_str)
                .and_then(parse_decimal)
        })
        .unwrap_or(0.0)
}

fn contains_forbidden_pal_progressive_marker(format: &str) -> bool {
    let bytes = format.as_bytes();
    bytes.windows(3).any(|window| {
        (window[0].eq_ignore_ascii_case(&b'p') && window[1] == b'2' && window[2] == b'5')
            || (window[0] == b'2' && window[1] == b'5' && window[2].eq_ignore_ascii_case(&b'p'))
    })
}

pub fn export_profile_is_forbidden_pal_progressive(values: &Value) -> bool {
    let fps = value_fps(values);
    let field_order = value_str(values, "field_order").to_ascii_lowercase();
    let format = value_str(values, "format").to_ascii_lowercase();
    let single_rate_pal = (fps - 25.0).abs() < 0.01;
    let explicit_forbidden = contains_forbidden_pal_progressive_marker(&format);
    let explicit_i50 = format.contains("i50") || field_order.contains("upper");
    explicit_forbidden || (single_rate_pal && field_order == "progressive" && !explicit_i50)
}

pub fn validate_export_profile(id: &str, name: &str, values: &Value) -> Result<(), String> {
    if export_profile_is_forbidden_pal_progressive(values) {
        return Err(
            "PAL single-rate progressive export nije dozvoljen; koristi p50 ili i50.".into(),
        );
    }
    let _ = (id, name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_catalog_presets_are_valid() {
        let presets = builtin_export_presets();
        assert!(!presets.is_empty());
        for preset in presets {
            validate_export_profile(&preset.id, &preset.name, &preset.values).unwrap();
        }
    }

    #[test]
    fn rejects_single_rate_pal_progressive_with_dot_or_comma_decimal() {
        let dot = json!({
            "format": "PAL single-rate progressive",
            "fps": "25.0",
            "field_order": "progressive"
        });
        let comma = json!({
            "format": "PAL single-rate progressive",
            "fps": "25,0",
            "field_order": "progressive"
        });

        assert!(export_profile_is_forbidden_pal_progressive(&dot));
        assert!(export_profile_is_forbidden_pal_progressive(&comma));
    }

    #[test]
    fn allows_i50_carried_as_25_frame_rate() {
        let values = export_profile_values(
            "HD 1080i50",
            25.0,
            1920,
            1080,
            "upper_first",
            "rec709",
            "mxf_op1a",
            "mpeg2_422_50mbit",
            48000,
            2,
        );

        assert!(!export_profile_is_forbidden_pal_progressive(&values));
    }
}
