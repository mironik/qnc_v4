//! Resolve Story keyboard bindings from host catalog + DB user overrides.
//!
//! Sources (required — no hardcoded chords in Rust):
//! 1. Host catalog: `GET /api/shell/keyboard-shortcuts`
//! 2. User overrides from SQLite: `GET /api/settings/keyboard-shortcuts`

use std::collections::HashMap;

use serde_json::Value;

pub(crate) const STORYBOARD_SHORTCUT_SCOPE: &str = "storyboard";
pub(crate) const PROJECT_SHORTCUT_SCOPE: &str = "project";

#[derive(Debug, Clone)]
pub struct KeyChord {
    pub code: Option<String>,
    pub key: Option<String>,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl KeyChord {
    pub fn display(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        let key = self
            .code
            .as_deref()
            .map(code_to_label)
            .or_else(|| self.key.as_deref().map(key_to_label))
            .unwrap_or("?");
        parts.push(key.to_string());
        parts.join("+")
    }
}

#[derive(Debug, Clone, Default)]
pub struct StoryBindings {
    /// action_id → chords
    pub by_action: HashMap<String, Vec<KeyChord>>,
    #[allow(dead_code)]
    pub labels: HashMap<String, String>,
    #[allow(dead_code)]
    pub source: String,
}

impl StoryBindings {
    pub fn empty() -> Self {
        Self {
            by_action: HashMap::new(),
            labels: HashMap::new(),
            source: "empty".into(),
        }
    }

    pub fn from_catalog(catalog: &Value, user: &Value, scope: &str) -> Self {
        let user_root = user.get("user").unwrap_or(user);
        let preset_id = user_root
            .get("active_preset")
            .and_then(|v| v.as_str())
            .or_else(|| catalog.get("active_preset").and_then(|v| v.as_str()))
            .unwrap_or("default");

        let mut labels = HashMap::new();
        if let Some(actions) = catalog.get("actions").and_then(|v| v.as_object()) {
            for (id, meta) in actions {
                if let Some(label) = meta.get("label").and_then(|v| v.as_str()) {
                    labels.insert(id.clone(), label.to_string());
                }
            }
        }

        let mut by_action = HashMap::new();
        if let Some(scope_map) = catalog_scope_map(catalog, preset_id, scope) {
            for (action_id, bindings) in scope_map {
                let chords: Vec<KeyChord> = bindings
                    .as_array()
                    .map(|arr| arr.iter().filter_map(parse_chord).collect())
                    .unwrap_or_default();
                if !chords.is_empty() {
                    by_action.insert(action_id.clone(), chords);
                }
            }
        }

        if let Some(scope_map) = user_root
            .get("bindings")
            .and_then(|b| b.get(preset_id))
            .and_then(|b| b.get(scope))
            .and_then(|v| v.as_object())
        {
            for (action_id, bindings) in scope_map {
                let chords: Vec<KeyChord> = bindings
                    .as_array()
                    .map(|arr| arr.iter().filter_map(parse_chord).collect())
                    .unwrap_or_default();
                if !chords.is_empty() {
                    by_action.insert(action_id.clone(), chords);
                } else {
                    // Empty override = unbound (do not keep catalog chord).
                    by_action.remove(action_id);
                }
            }
        }

        Self {
            by_action,
            labels,
            source: format!("catalog+db:{preset_id}/{scope}"),
        }
    }

    #[allow(dead_code)]
    pub fn label<'a>(&'a self, action_id: &'a str) -> &'a str {
        self.labels
            .get(action_id)
            .map(|s| s.as_str())
            .unwrap_or(action_id)
    }

    pub fn chord_hint(&self, action_id: &str) -> String {
        let Some(chords) = self.by_action.get(action_id) else {
            return String::new();
        };
        if chords.is_empty() {
            return String::new();
        }
        chords
            .iter()
            .map(KeyChord::display)
            .collect::<Vec<_>>()
            .join(" / ")
    }

    pub fn matching_actions(&self, key: egui::Key, modifiers: egui::Modifiers) -> Vec<&str> {
        let mut out = Vec::new();
        for (action_id, chords) in &self.by_action {
            if chords.iter().any(|c| chord_matches(c, key, modifiers)) {
                out.push(action_id.as_str());
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

fn catalog_scope_map<'a>(
    catalog: &'a Value,
    preset_id: &str,
    scope: &str,
) -> Option<&'a serde_json::Map<String, Value>> {
    let presets = catalog.get("presets")?;
    presets
        .get(preset_id)
        .and_then(|preset| preset.get(scope))
        .and_then(|v| v.as_object())
        .or_else(|| {
            presets
                .get("default")
                .and_then(|preset| preset.get(scope))
                .and_then(|v| v.as_object())
        })
}

fn parse_chord(v: &Value) -> Option<KeyChord> {
    let obj = v.as_object()?;
    Some(KeyChord {
        code: obj
            .get("code")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        key: obj
            .get("key")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        shift: obj.get("shift").and_then(|x| x.as_bool()).unwrap_or(false),
        ctrl: obj
            .get("ctrl")
            .or_else(|| obj.get("ctrlKey"))
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        alt: obj.get("alt").and_then(|x| x.as_bool()).unwrap_or(false),
    })
}

fn chord_matches(chord: &KeyChord, key: egui::Key, modifiers: egui::Modifiers) -> bool {
    if chord.shift != modifiers.shift {
        return false;
    }
    if chord.ctrl != (modifiers.ctrl || modifiers.command) {
        return false;
    }
    if chord.alt != modifiers.alt {
        return false;
    }
    if let Some(code) = chord.code.as_deref() {
        if egui_key_code(key) == Some(code) {
            return true;
        }
    }
    if let Some(k) = chord.key.as_deref() {
        if egui_key_char(key).is_some_and(|c| c.eq_ignore_ascii_case(k)) {
            return true;
        }
        if egui_key_name(key).is_some_and(|n| n.eq_ignore_ascii_case(k)) {
            return true;
        }
    }
    false
}

fn code_to_label(code: &str) -> &str {
    match code {
        "KeyA" => "A",
        "KeyB" => "B",
        "KeyC" => "C",
        "KeyD" => "D",
        "KeyE" => "E",
        "KeyF" => "F",
        "KeyG" => "G",
        "KeyH" => "H",
        "KeyI" => "I",
        "KeyJ" => "J",
        "KeyK" => "K",
        "KeyL" => "L",
        "KeyM" => "M",
        "KeyN" => "N",
        "KeyO" => "O",
        "KeyP" => "P",
        "KeyQ" => "Q",
        "KeyR" => "R",
        "KeyS" => "S",
        "KeyT" => "T",
        "KeyU" => "U",
        "KeyV" => "V",
        "KeyW" => "W",
        "KeyX" => "X",
        "KeyY" => "Y",
        "KeyZ" => "Z",
        "Space" => "Space",
        "BracketLeft" | "OpenBracket" => "[",
        "BracketRight" | "CloseBracket" => "]",
        "Slash" => "/",
        "F1" => "F1",
        other => other,
    }
}

fn key_to_label(key: &str) -> &str {
    match key {
        " " => "Space",
        "ArrowLeft" => "←",
        "ArrowRight" => "→",
        "ArrowUp" => "↑",
        "ArrowDown" => "↓",
        other => other,
    }
}

fn egui_key_name(key: egui::Key) -> Option<&'static str> {
    Some(match key {
        egui::Key::Tab => "Tab",
        egui::Key::Escape => "Escape",
        egui::Key::Enter => "Enter",
        egui::Key::Space => "Space",
        egui::Key::Delete => "Delete",
        egui::Key::Backspace => "Backspace",
        egui::Key::ArrowLeft => "ArrowLeft",
        egui::Key::ArrowRight => "ArrowRight",
        egui::Key::ArrowUp => "ArrowUp",
        egui::Key::ArrowDown => "ArrowDown",
        egui::Key::F1 => "F1",
        egui::Key::Slash => "/",
        egui::Key::OpenBracket => "[",
        egui::Key::CloseBracket => "]",
        _ => return None,
    })
}

fn egui_key_code(key: egui::Key) -> Option<&'static str> {
    Some(match key {
        egui::Key::A => "KeyA",
        egui::Key::B => "KeyB",
        egui::Key::C => "KeyC",
        egui::Key::D => "KeyD",
        egui::Key::E => "KeyE",
        egui::Key::F => "KeyF",
        egui::Key::G => "KeyG",
        egui::Key::H => "KeyH",
        egui::Key::I => "KeyI",
        egui::Key::J => "KeyJ",
        egui::Key::K => "KeyK",
        egui::Key::L => "KeyL",
        egui::Key::M => "KeyM",
        egui::Key::N => "KeyN",
        egui::Key::O => "KeyO",
        egui::Key::P => "KeyP",
        egui::Key::Q => "KeyQ",
        egui::Key::R => "KeyR",
        egui::Key::S => "KeyS",
        egui::Key::T => "KeyT",
        egui::Key::U => "KeyU",
        egui::Key::V => "KeyV",
        egui::Key::W => "KeyW",
        egui::Key::X => "KeyX",
        egui::Key::Y => "KeyY",
        egui::Key::Z => "KeyZ",
        egui::Key::Space => "Space",
        egui::Key::Escape => "Escape",
        egui::Key::Enter => "Enter",
        egui::Key::Delete => "Delete",
        egui::Key::Backspace => "Backspace",
        egui::Key::ArrowLeft => "ArrowLeft",
        egui::Key::ArrowRight => "ArrowRight",
        egui::Key::ArrowUp => "ArrowUp",
        egui::Key::ArrowDown => "ArrowDown",
        egui::Key::Tab => "Tab",
        egui::Key::F1 => "F1",
        egui::Key::Slash => "Slash",
        egui::Key::OpenBracket => "BracketLeft",
        egui::Key::CloseBracket => "BracketRight",
        _ => return None,
    })
}

fn egui_key_char(key: egui::Key) -> Option<&'static str> {
    Some(match key {
        egui::Key::A => "a",
        egui::Key::B => "b",
        egui::Key::C => "c",
        egui::Key::D => "d",
        egui::Key::E => "e",
        egui::Key::F => "f",
        egui::Key::G => "g",
        egui::Key::H => "h",
        egui::Key::I => "i",
        egui::Key::J => "j",
        egui::Key::K => "k",
        egui::Key::L => "l",
        egui::Key::M => "m",
        egui::Key::N => "n",
        egui::Key::O => "o",
        egui::Key::P => "p",
        egui::Key::Q => "q",
        egui::Key::R => "r",
        egui::Key::S => "s",
        egui::Key::T => "t",
        egui::Key::U => "u",
        egui::Key::V => "v",
        egui::Key::W => "w",
        egui::Key::X => "x",
        egui::Key::Y => "y",
        egui::Key::Z => "z",
        egui::Key::Space => " ",
        egui::Key::Tab => "Tab",
        egui::Key::Slash => "/",
        egui::Key::OpenBracket => "[",
        egui::Key::CloseBracket => "]",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn catalog_maps_mark_in_out() {
        let catalog = json!({
            "active_preset": "default",
            "actions": {
                "mark_in": { "label": "Mark IN" },
                "mark_out": { "label": "Mark OUT" }
            },
            "presets": {
                "default": {
                    "storyboard": {
                        "mark_in": [{ "code": "KeyI", "shift": false }],
                        "mark_out": [{ "code": "KeyO", "shift": false }],
                        "select_mark_in": [{ "code": "KeyI", "ctrl": true, "shift": false }]
                    }
                }
            }
        });
        let bindings = StoryBindings::from_catalog(&catalog, &Value::Null, "storyboard");
        assert_eq!(bindings.chord_hint("mark_in"), "I");
        assert_eq!(bindings.chord_hint("mark_out"), "O");
        assert_eq!(bindings.chord_hint("select_mark_in"), "Ctrl+I");

        let plain = egui::Modifiers::NONE;
        let mut ctrl = egui::Modifiers::NONE;
        ctrl.ctrl = true;
        assert_eq!(
            bindings.matching_actions(egui::Key::I, plain),
            vec!["mark_in"]
        );
        assert_eq!(
            bindings.matching_actions(egui::Key::I, ctrl),
            vec!["select_mark_in"]
        );
        assert_eq!(
            bindings.matching_actions(egui::Key::O, plain),
            vec!["mark_out"]
        );
    }

    #[test]
    fn user_override_replaces_chord() {
        let catalog = json!({
            "active_preset": "default",
            "presets": {
                "default": {
                    "storyboard": {
                        "mark_in": [{ "code": "KeyI", "shift": false }]
                    }
                }
            }
        });
        let user = json!({
            "user": {
                "active_preset": "default",
                "bindings": {
                    "default": {
                        "storyboard": {
                            "mark_in": [{ "code": "KeyE", "shift": false }]
                        }
                    }
                }
            }
        });
        let bindings = StoryBindings::from_catalog(&catalog, &user, "storyboard");
        assert_eq!(
            bindings.matching_actions(egui::Key::E, egui::Modifiers::NONE),
            vec!["mark_in"]
        );
        assert!(bindings
            .matching_actions(egui::Key::I, egui::Modifiers::NONE)
            .is_empty());
    }

    #[test]
    fn missing_scope_uses_default_preset_scope_from_catalog() {
        let catalog = json!({
            "active_preset": "edius",
            "presets": {
                "default": {
                    "project": {
                        "project_open_selected": [{ "key": "Enter" }]
                    }
                },
                "edius": {
                    "storyboard": {
                        "mark_in": [{ "code": "KeyI" }]
                    }
                }
            }
        });
        let bindings = StoryBindings::from_catalog(&catalog, &Value::Null, PROJECT_SHORTCUT_SCOPE);

        assert_eq!(
            bindings.matching_actions(egui::Key::Enter, egui::Modifiers::NONE),
            vec!["project_open_selected"]
        );
    }

    #[test]
    fn empty_bindings_have_no_chords() {
        let b = StoryBindings::empty();
        assert!(b.by_action.is_empty());
        assert_eq!(b.chord_hint("mark_in"), "");
    }
}
