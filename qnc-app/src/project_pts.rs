//! PTS Advanced constants — parity with web `project-template-settings`.

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

pub const INGEST_PROFILES: &[(&str, &str)] = &[("field", "Teren"), ("house", "TV kuća")];

pub const PROXY_POLICIES: &[(&str, &str)] = &[
    ("generate_if_missing", "Generiraj ako nema"),
    ("copy_to_project", "Kopiraj u projekt"),
    ("use_house_media", "Kućni medij"),
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

fn preset_values(
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

/// Built-in export presets (same ids as web).
pub fn builtin_export_presets() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "xdcam_hd422_50i",
            "XDCAM HD422 50i (PAL)",
            preset_values(
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
            ),
        ),
        (
            "xdcam_hd422_60i",
            "XDCAM HD422 60i (NTSC)",
            preset_values(
                "HD 1080i60",
                29.97,
                1920,
                1080,
                "upper_first",
                "rec709",
                "mxf_op1a",
                "mpeg2_422_50mbit",
                48000,
                2,
            ),
        ),
        (
            "xdcam_hd422_30p",
            "XDCAM HD422 30p (NTSC)",
            preset_values(
                "HD 1080p30",
                29.97,
                1920,
                1080,
                "progressive",
                "rec709",
                "mxf_op1a",
                "mpeg2_422_50mbit",
                48000,
                2,
            ),
        ),
        (
            "h264_1080p50",
            "H.264 1080p50 (PAL)",
            preset_values(
                "HD 1080p50",
                50.0,
                1920,
                1080,
                "progressive",
                "rec709",
                "mp4",
                "h264",
                48000,
                2,
            ),
        ),
        (
            "h264_1080p30",
            "H.264 1080p29.97 (NTSC)",
            preset_values(
                "HD 1080p30",
                29.97,
                1920,
                1080,
                "progressive",
                "rec709",
                "mp4",
                "h264",
                48000,
                2,
            ),
        ),
        (
            "h264_1080p60",
            "H.264 1080p59.94 (NTSC)",
            preset_values(
                "HD 1080p60",
                59.94,
                1920,
                1080,
                "progressive",
                "rec709",
                "mp4",
                "h264",
                48000,
                2,
            ),
        ),
        (
            "dnxhd_hq",
            "DNxHD HQ (Avid)",
            preset_values(
                "HD 1080i50",
                25.0,
                1920,
                1080,
                "upper_first",
                "rec709",
                "mxf_op1a",
                "dnxhd_hq",
                48000,
                2,
            ),
        ),
    ]
}

fn value_str<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn value_fps(value: &Value) -> f64 {
    value
        .get("fps")
        .and_then(Value::as_f64)
        .or_else(|| value.get("fps").and_then(Value::as_i64).map(|v| v as f64))
        .or_else(|| value.get("fps").and_then(Value::as_str)?.parse().ok())
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

pub fn custom_export_presets(effective: &Value) -> Vec<(String, String, Value)> {
    effective
        .get("export")
        .and_then(|e| e.get("custom_presets"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    let id = p.get("id")?.as_str()?.to_string();
                    if id.is_empty() {
                        return None;
                    }
                    let name = p
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or(&id)
                        .to_string();
                    let values = p.get("values").cloned().unwrap_or(json!({}));
                    validate_export_profile(&id, &name, &values).ok()?;
                    Some((id, name, values))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn resolve_export_preset_values(effective: &Value, id: &str) -> Option<Value> {
    if id.is_empty() || id == "manual" {
        return None;
    }
    for (pid, _, values) in builtin_export_presets() {
        if pid == id {
            return Some(values);
        }
    }
    custom_export_presets(effective)
        .into_iter()
        .find(|(pid, _, _)| pid == id)
        .map(|(_, _, v)| v)
}

/// `settings_override.export` patch for preset select (web `applyExportPreset`).
pub fn export_preset_override_patch(effective: &Value, preset_id: &str) -> Value {
    let id = if preset_id.trim().is_empty() {
        "manual"
    } else {
        preset_id.trim()
    };
    if id == "manual" {
        return json!({ "export": { "preset": "manual" } });
    }
    let mut export = json!({ "preset": id });
    if let Some(values) = resolve_export_preset_values(effective, id) {
        if let (Some(obj), Some(vals)) = (export.as_object_mut(), values.as_object()) {
            for (k, v) in vals {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    json!({ "export": export })
}

pub fn normalize_export_mode(mode: &str) -> String {
    match mode {
        "proxy_fast" => "original".into(),
        "broadcast_mxf" => "xdcam".into(),
        other if !other.is_empty() => other.into(),
        _ => "xml_master".into(),
    }
}

pub fn path_num(effective: &Value, path: &[&str]) -> Option<f64> {
    let mut cur = effective;
    for p in path {
        cur = cur.get(*p)?;
    }
    cur.as_f64()
        .or_else(|| cur.as_i64().map(|i| i as f64))
        .or_else(|| cur.as_str()?.parse().ok())
}

pub fn path_i64(effective: &Value, path: &[&str], default: i64) -> i64 {
    path_num(effective, path)
        .map(|n| n.round() as i64)
        .unwrap_or(default)
}

pub fn fps_display(effective: &Value, path: &[&str], default: f64) -> String {
    let n = path_num(effective, path).unwrap_or(default);
    if (n - n.round()).abs() < 0.001 {
        format!("{}", n.round() as i64)
    } else {
        format!("{n}")
    }
}

pub fn slug_preset_id(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let s = s.trim_matches('_').chars().take(40).collect::<String>();
    if s.is_empty() {
        format!("custom_{}", chrono_like_id())
    } else {
        format!("custom_{s}")
    }
}

fn chrono_like_id() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn module_sort_key(mod_row: &crate::api::ModuleRow) -> (i32, i64, String) {
    let tab = mod_row.tab_key();
    let position = mod_row.position.as_str();
    let bucket = if position == "first" || tab == "project" {
        -1
    } else if position == "last" || tab == "preview" || tab == "export" {
        1
    } else {
        0
    };
    (
        bucket,
        mod_row.priority,
        mod_row.display_label().to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_export_presets_have_no_forbidden_pal_progressive() {
        let ids = builtin_export_presets()
            .into_iter()
            .map(|(id, name, values)| {
                validate_export_profile(id, name, &values).unwrap();
                (id.to_string(), values)
            })
            .collect::<Vec<_>>();

        assert!(!ids
            .iter()
            .any(|(_, values)| export_profile_is_forbidden_pal_progressive(values)));
    }

    #[test]
    fn forbidden_pal_progressive_export_is_rejected() {
        let values = preset_values(
            "PAL single-rate progressive",
            25.0,
            1920,
            1080,
            "progressive",
            "rec709",
            "mp4",
            "h264",
            48000,
            2,
        );

        assert!(
            validate_export_profile("single_rate_pal", "H.264 single-rate PAL", &values).is_err()
        );
    }

    #[test]
    fn i50_uses_25_frame_rate_but_is_not_forbidden_pal_progressive() {
        let values = preset_values(
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
        assert!(validate_export_profile("xdcam_hd422_50i", "XDCAM HD422 50i", &values).is_ok());
    }
}
