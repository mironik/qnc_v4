//! Resolve Storyboard keyboard bindings from QNC catalog (host), not hardcoded keys.

use serde_json::Value;

use crate::focus::FocusTarget;

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
    pub by_action: std::collections::HashMap<String, Vec<KeyChord>>,
    pub labels: std::collections::HashMap<String, String>,
}

impl StoryBindings {
    pub fn from_catalog(catalog: &Value, user: &Value, scope: &str) -> Self {
        let preset_id = user
            .get("user")
            .and_then(|u| u.get("active_preset"))
            .or_else(|| user.get("active_preset"))
            .and_then(|v| v.as_str())
            .or_else(|| catalog.get("active_preset").and_then(|v| v.as_str()))
            .unwrap_or("default");

        let mut labels = std::collections::HashMap::new();
        if let Some(actions) = catalog.get("actions").and_then(|v| v.as_object()) {
            for (id, meta) in actions {
                if let Some(label) = meta.get("label").and_then(|v| v.as_str()) {
                    labels.insert(id.clone(), label.to_string());
                }
            }
        }

        let mut by_action = std::collections::HashMap::new();
        if let Some(scope_map) = catalog
            .get("presets")
            .and_then(|p| p.get(preset_id))
            .and_then(|p| p.get(scope))
            .and_then(|v| v.as_object())
        {
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

        // User override for this scope (if present).
        if let Some(scope_map) = user
            .get("user")
            .or(Some(user))
            .and_then(|u| u.get("bindings"))
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
                }
            }
        }

        Self { by_action, labels }
    }

    pub fn label<'a>(&'a self, action_id: &'a str) -> &'a str {
        self.labels
            .get(action_id)
            .map(|s| s.as_str())
            .unwrap_or(action_id)
    }

    /// Human-readable binding(s) for an action from the catalog (for status / help).
    pub fn chord_hint(&self, action_id: &str) -> String {
        let Some(chords) = self.by_action.get(action_id) else {
            return action_id.to_string();
        };
        if chords.is_empty() {
            return action_id.to_string();
        }
        chords
            .iter()
            .map(KeyChord::display)
            .collect::<Vec<_>>()
            .join(" / ")
    }

    /// All catalog actions that match this physical key + modifiers.
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

    /// Pick one action: keys come only from catalog; focus resolves collisions.
    pub fn resolve_action(
        &self,
        key: egui::Key,
        modifiers: egui::Modifiers,
        focus: &FocusTarget,
    ) -> Option<&str> {
        let matches = self.matching_actions(key, modifiers);
        if matches.is_empty() {
            return None;
        }
        if matches.len() == 1 {
            return Some(matches[0]);
        }

        let has = |id: &str| matches.iter().any(|m| *m == id);

        // Esc: clear_focus vs close_player — both may bind Escape in catalog.
        if has("clear_focus") && has("close_player") {
            return Some(if matches!(focus, FocusTarget::Playhead) {
                "close_player"
            } else {
                "clear_focus"
            });
        }

        // Delete: delete_marker vs delete_part — focus decides.
        if has("delete_marker") && has("delete_part") {
            return Some(if matches!(focus, FocusTarget::Marker { .. }) {
                "delete_marker"
            } else {
                "delete_part"
            });
        }

        // Prefer more specific select/focus actions if somehow colliding.
        for preferred in [
            "delete_marker",
            "clear_focus",
            "select_mark_in",
            "select_mark_out",
            "select_marker",
            "mark_in_fit_duration",
            "focus_empty_slot",
            "step_next_marker_slot",
            "step_prev_marker_slot",
            "overwrite_cover",
            "quick_overwrite_cover",
            "undo_last",
            "toggle_cheatsheet",
            "toggle_source_wrap",
            "focus_next",
            "focus_prev",
            "add_ton_segment",
            "add_off_segment",
        ] {
            if has(preferred) {
                return Some(preferred);
            }
        }

        Some(matches[0])
    }
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
        return egui_key_code(key) == Some(code);
    }
    if let Some(k) = chord.key.as_deref() {
        if egui_key_char(key).is_some_and(|c| c.eq_ignore_ascii_case(k)) {
            return true;
        }
        return egui_key_name(key).is_some_and(|n| n.eq_ignore_ascii_case(k));
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
