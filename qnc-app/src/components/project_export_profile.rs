use serde_json::{json, Value};

use crate::component_runtime::ComponentBackendCommand;

use super::ProjectCommandComponent;

pub(crate) struct ProjectExportProfileComponent;

impl ProjectExportProfileComponent {
    pub fn apply_preset(
        request_id: u64,
        effective_settings: &Value,
        preset_id: &str,
    ) -> ComponentBackendCommand {
        ProjectCommandComponent::merge_settings_override(
            request_id,
            "export.preset.apply",
            crate::project_pts::export_preset_override_patch(effective_settings, preset_id),
        )
    }

    pub fn save_custom_preset(
        request_id: u64,
        effective_settings: &Value,
        name: &str,
    ) -> Result<ComponentBackendCommand, String> {
        let patch = custom_export_preset_patch(effective_settings, name)?;
        Ok(ProjectCommandComponent::merge_settings_override(
            request_id,
            "export.preset.save",
            patch,
        ))
    }
}

fn custom_export_preset_patch(effective: &Value, name: &str) -> Result<Value, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Unesi naziv preseta.".into());
    }
    let export = effective.get("export").cloned().unwrap_or(Value::Null);
    let fps = crate::project_pts::path_num(effective, &["export", "fps"])
        .ok_or_else(|| "Export FPS nije postavljen.".to_string())?;
    let format = required_export_str(&export, "format", "Export format nije postavljen.")?;
    let width = required_export_i64(effective, "width", "Export width nije postavljen.")?;
    let height = required_export_i64(effective, "height", "Export height nije postavljen.")?;
    let field_order = required_export_str(
        &export,
        "field_order",
        "Export field order nije postavljen.",
    )?;
    let color_space = required_export_str(
        &export,
        "color_space",
        "Export color space nije postavljen.",
    )?;
    let container = required_export_str(&export, "container", "Export container nije postavljen.")?;
    let video_codec = required_export_str(
        &export,
        "video_codec",
        "Export video codec nije postavljen.",
    )?;
    let audio_sample_rate = required_export_i64(
        effective,
        "audio_sample_rate",
        "Export audio sample rate nije postavljen.",
    )?;
    let audio_channels = required_export_i64(
        effective,
        "audio_channels",
        "Export audio channels nije postavljen.",
    )?;
    let id = crate::project_pts::slug_preset_id(name);
    let preset = json!({
        "id": id,
        "name": name,
        "values": {
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
        }
    });
    let values = preset.get("values").cloned().unwrap_or(Value::Null);
    crate::project_pts::validate_export_profile(&id, name, &values)?;

    let mut existing: Vec<Value> = export
        .get("custom_presets")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let preset_id = preset
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if let Some(slot) = existing
        .iter_mut()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(preset_id.as_str()))
    {
        *slot = preset.clone();
    } else {
        existing.push(preset.clone());
    }

    let mut export_patch = json!({
        "custom_presets": existing,
        "preset": preset_id,
    });
    if let (Some(obj), Some(vals)) = (export_patch.as_object_mut(), values.as_object()) {
        for (key, value) in vals {
            obj.insert(key.clone(), value.clone());
        }
    }
    Ok(json!({ "export": export_patch }))
}

fn required_export_str(export: &Value, key: &str, error: &str) -> Result<String, String> {
    export
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| error.to_string())
}

fn required_export_i64(effective: &Value, key: &str, error: &str) -> Result<i64, String> {
    crate::project_pts::path_num(effective, &["export", key])
        .map(|value| value.round() as i64)
        .ok_or_else(|| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_preset_requires_explicit_export_fps() {
        let effective = json!({ "export": { "format": "HD 1080i50" } });

        let err = custom_export_preset_patch(&effective, "News").unwrap_err();

        assert!(err.contains("Export FPS"));
    }

    #[test]
    fn custom_preset_patch_keeps_explicit_export_fps() {
        let effective = json!({
            "export": {
                "format": "HD 1080p50",
                "fps": "50,0",
                "width": 1920,
                "height": 1080,
                "field_order": "progressive",
                "color_space": "rec709",
                "container": "mxf_op1a",
                "video_codec": "mpeg2_422_50mbit",
                "audio_sample_rate": 48000,
                "audio_channels": 2
            }
        });

        let patch = custom_export_preset_patch(&effective, "News").unwrap();

        assert_eq!(
            patch
                .get("export")
                .and_then(|v| v.get("fps"))
                .and_then(Value::as_f64),
            Some(50.0)
        );
    }
}
