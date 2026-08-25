//! PTS Advanced helpers. Export profile definitions live in `qnc-service-contracts`.

use serde_json::{json, Value};

use crate::locale_number::parse_decimal;
pub use qnc_service_contracts::export_profile::{
    AUDIO_CHANNELS, AUDIO_RATES, COLOR_SPACE, CONTAINERS, EXPORT_MODES, FIELD_ORDER, FPS_OPTIONS,
    INGEST_PROFILES, INPUT_FORMATS, ORIGINAL_POLICIES, PROXY_POLICIES, VIDEO_CODECS,
};

/// Built-in export presets (same ids as web).
pub fn builtin_export_presets() -> Vec<(String, String, Value)> {
    qnc_service_contracts::export_profile::builtin_export_presets()
        .into_iter()
        .map(|preset| (preset.id, preset.name, preset.values))
        .collect()
}

pub fn validate_export_profile(id: &str, name: &str, values: &Value) -> Result<(), String> {
    qnc_service_contracts::export_profile::validate_export_profile(id, name, values)
}

#[allow(dead_code)]
pub fn export_profile_is_forbidden_pal_progressive(values: &Value) -> bool {
    qnc_service_contracts::export_profile::export_profile_is_forbidden_pal_progressive(values)
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
        .or_else(|| cur.as_str().and_then(parse_decimal))
}

pub fn fps_display_optional(effective: &Value, path: &[&str]) -> String {
    path_num(effective, path)
        .map(fps_value_display)
        .unwrap_or_default()
}

pub fn int_display_optional(effective: &Value, path: &[&str]) -> String {
    path_num(effective, path)
        .map(|value| (value.round() as i64).to_string())
        .unwrap_or_default()
}

fn fps_value_display(n: f64) -> String {
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
                validate_export_profile(&id, &name, &values).unwrap();
                (id.to_string(), values)
            })
            .collect::<Vec<_>>();

        assert!(!ids
            .iter()
            .any(|(_, values)| export_profile_is_forbidden_pal_progressive(values)));
    }

    #[test]
    fn forbidden_pal_progressive_export_is_rejected() {
        let values = qnc_service_contracts::export_profile::export_profile_values(
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
        let values = qnc_service_contracts::export_profile::export_profile_values(
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

    #[test]
    fn optional_fps_display_does_not_invent_missing_value() {
        let empty = json!({});
        let fps = json!({ "export": { "fps": "50,0" } });

        assert_eq!(fps_display_optional(&empty, &["export", "fps"]), "");
        assert_eq!(fps_display_optional(&fps, &["export", "fps"]), "50");
        assert_eq!(int_display_optional(&empty, &["export", "width"]), "");
    }
}
