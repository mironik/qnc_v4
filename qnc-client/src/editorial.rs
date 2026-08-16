//! Source I/O marks → Story virtual segment (tonovi / offovi).

use serde_json::Value;

use crate::api::HostClient;
use crate::focus::seconds_to_frame;

#[derive(Debug, Clone, Default)]
pub struct SourceMarks {
    pub clip_id: Option<String>,
    pub mark_in: Option<i64>,
    pub mark_out: Option<i64>,
}

impl SourceMarks {
    pub fn set_in(&mut self, clip_id: &str, frame: i64) {
        self.clip_id = Some(clip_id.to_string());
        self.mark_in = Some(frame.max(0));
        if self.mark_out.is_some_and(|o| o <= frame) {
            self.mark_out = None;
        }
    }

    pub fn set_out(&mut self, clip_id: &str, frame: i64) {
        let frame = frame.max(0);
        if self.clip_id.as_deref() != Some(clip_id) {
            self.clip_id = Some(clip_id.to_string());
            self.mark_in = None;
        }
        self.mark_out = Some(frame);
    }

    pub fn summary(&self) -> String {
        match (self.clip_id.as_deref(), self.mark_in, self.mark_out) {
            (Some(c), Some(i), Some(o)) => format!("IN={i}f OUT={o}f clip={c}"),
            (Some(c), Some(i), None) => format!("IN={i}f OUT=? clip={c}"),
            (Some(c), None, Some(o)) => format!("IN=? OUT={o}f clip={c}"),
            _ => "IN/OUT nisu postavljeni".into(),
        }
    }
}

/// Map keyboard action → DB kind for Story parts.
pub fn kind_for_action(action_id: &str) -> Option<&'static str> {
    match action_id {
        "add_ton_segment" => Some("tonovi"),
        "add_off_segment" => Some("offovi"),
        _ => None,
    }
}

/// Create tonovi/offovi part from I/O marks (preferred) or currently selected shot in DB.
/// Returns (status message, created part_id for undo).
pub fn create_segment(
    host: &HostClient,
    project_id: &str,
    kind: &str,
    marks: &SourceMarks,
) -> Result<(String, String), String> {
    if let (Some(clip_id), Some(inn), Some(out)) =
        (marks.clip_id.as_deref(), marks.mark_in, marks.mark_out)
    {
        if out <= inn {
            return Err("OUT mora biti nakon IN".into());
        }
        let snap = host.create_part_ex_frames(
            project_id,
            kind,
            None,
            Some(clip_id),
            Some(inn),
            Some(out),
        )?;
        let part_id = snap
            .get("selected_part_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        return Ok((format!("{kind} segment iz IN/OUT"), part_id));
    }
    let snap = host.create_part(project_id, kind, None)?;
    let part_id = snap
        .get("selected_part_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok((
        format!("{kind} iz odabranog kadra (nema potpunog IN/OUT)"),
        part_id,
    ))
}

/// Resolve clip_id for mark-in/out from Story snapshot selection.
pub fn clip_id_from_state(state: &Value) -> Option<String> {
    let selected = state
        .get("selected_shot_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;

    if let Some(clip) = state
        .get("all_clips")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|c| c.get("root_shot_id").and_then(Value::as_str) == Some(selected))
        .and_then(|c| c.get("clip_id").and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(clip.to_string());
    }

    if let Some(rest) = selected
        .strip_suffix("_root")
        .or_else(|| selected.strip_prefix("root_"))
    {
        let id = rest.trim();
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }

    state
        .get("virtual_shots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s.get("shot_id").and_then(Value::as_str) == Some(selected))
        .and_then(|s| s.get("clip_id").and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Absolute source frame for a part at wrap playhead (from story_state parts).
pub fn source_frame_at_part(
    state: &Value,
    part_id: &str,
    local_frame: i64,
) -> Option<(String, i64)> {
    let part = state
        .get("parts")
        .and_then(Value::as_array)?
        .iter()
        .find(|p| p.get("part_id").and_then(Value::as_str) == Some(part_id))?;
    let clip_id = part
        .get("clip_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let in_frame = part
        .get("in_frame")
        .and_then(Value::as_i64)
        .filter(|frame| *frame >= 0)
        .or_else(|| {
            let fps = json_fps(part, "fps")?;
            let in_sec = part.get("in_seconds").and_then(Value::as_f64)?;
            Some(seconds_to_frame(in_sec, fps))
        })?;
    Some((clip_id, in_frame + local_frame.max(0)))
}

pub fn selected_slot_duration_frames(state: &Value) -> Option<(String, i64)> {
    let selected = state
        .get("selected_slot_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let slot = state
        .get("marker_slots")
        .and_then(Value::as_array)?
        .iter()
        .find(|s| s.get("slot_id").and_then(Value::as_str) == Some(selected))?;
    let frames = slot
        .get("duration_frames")
        .and_then(Value::as_i64)
        .filter(|d| *d > 0)
        .or_else(|| {
            let a = slot.get("start_frame").and_then(Value::as_i64)?;
            let b = slot.get("end_frame").and_then(Value::as_i64)?;
            Some((b - a).max(0))
        })
        .or_else(|| duration_from_slot_seconds(slot, timeline_fps_from_state(state)?))
        .filter(|d| *d > 0)?;
    Some((selected.to_string(), frames))
}

pub fn slot_duration_frames_by_id(state: &Value, slot_id: &str) -> Option<i64> {
    let slot = state
        .get("marker_slots")
        .and_then(Value::as_array)?
        .iter()
        .find(|s| s.get("slot_id").and_then(Value::as_str) == Some(slot_id))?;
    slot.get("duration_frames")
        .and_then(Value::as_i64)
        .filter(|d| *d > 0)
        .or_else(|| {
            let a = slot.get("start_frame").and_then(Value::as_i64)?;
            let b = slot.get("end_frame").and_then(Value::as_i64)?;
            Some((b - a).max(0))
        })
        .or_else(|| duration_from_slot_seconds(slot, timeline_fps_from_state(state)?))
}

fn json_fps(value: &Value, key: &str) -> Option<f64> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|fps| fps.is_finite() && *fps > 0.0)
}

fn timeline_fps_from_state(state: &Value) -> Option<f64> {
    json_fps(state, "timeline_fps").or_else(|| {
        state
            .get("timeline")
            .and_then(|timeline| json_fps(timeline, "timeline_fps"))
    })
}

fn duration_from_slot_seconds(slot: &Value, fps: f64) -> Option<i64> {
    let a = slot.get("start_sec").and_then(Value::as_f64)?;
    let b = slot.get("end_sec").and_then(Value::as_f64)?;
    Some(seconds_to_frame((b - a).max(0.0), fps))
}

/// Apply IN at `frame`, OUT = IN + selected slot duration (from SQLite selection).
pub fn apply_mark_in_fit_frames(
    marks: &mut SourceMarks,
    clip_id: &str,
    in_frame: i64,
    state: &Value,
) -> Result<(i64, i64), String> {
    let (_slot_id, dur) = selected_slot_duration_frames(state)
        .ok_or_else(|| "Nema odabranog slota / trajanja (fokus prazni slot)".to_string())?;
    let inn = in_frame.max(0);
    let out = inn + dur;
    marks.set_in(clip_id, inn);
    marks.set_out(clip_id, out);
    Ok((inn, out))
}

fn slot_covered(state: &Value, slot: &Value) -> bool {
    let sig = slot
        .get("slot_signature")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let idx = slot.get("slot_index").and_then(Value::as_i64);
    let covers = state.get("covers").and_then(Value::as_array);
    let Some(covers) = covers else {
        return false;
    };
    covers.iter().any(|c| {
        let c_sig = c
            .get("slot_signature")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if !sig.is_empty() && c_sig == sig {
            return true;
        }
        idx.is_some_and(|i| c.get("slot_index").and_then(Value::as_i64) == Some(i))
    })
}

/// First M–M slot without a cover (story snapshot).
pub fn first_empty_slot(state: &Value) -> Option<&Value> {
    state
        .get("marker_slots")
        .and_then(Value::as_array)?
        .iter()
        .find(|s| {
            s.get("slot_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .is_some()
                && !slot_covered(state, s)
        })
}

/// Create virtual shot from marks and place as cover on selected (or given) slot.
/// Returns (message, cover_id for undo).
pub fn create_cover_from_marks(
    host: &HostClient,
    project_id: &str,
    marks: &SourceMarks,
    state: &Value,
    slot_id_override: Option<&str>,
) -> Result<(String, String), String> {
    let (clip_id, inn, out) = match (marks.clip_id.as_deref(), marks.mark_in, marks.mark_out) {
        (Some(c), Some(i), Some(o)) if o > i => (c, i, o),
        _ => return Err("Pokrivalica treba IN/OUT (Shift+I nakon fokusa slota)".into()),
    };
    let slot_id = slot_id_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            state
                .get("selected_slot_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .ok_or_else(|| "Nema selected_slot_id".to_string())?;

    let slot_sig = state
        .get("marker_slots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s.get("slot_id").and_then(Value::as_str) == Some(slot_id.as_str()))
        .and_then(|s| s.get("slot_signature").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    let slot_index = state
        .get("marker_slots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s.get("slot_id").and_then(Value::as_str) == Some(slot_id.as_str()))
        .and_then(|s| s.get("slot_index").and_then(Value::as_i64));

    let resp = host.create_virtual_shot_from_frames(project_id, clip_id, inn, out)?;
    let shot_id = resp
        .get("shot")
        .and_then(|s| s.get("shot_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| "virtual-shot bez shot_id".to_string())?
        .to_string();
    let _ = host.shot_select(project_id, &shot_id)?;
    let snap = host.create_cover(project_id, &slot_id, Some(&shot_id), Some(clip_id))?;
    let cover_id = snap
        .get("covers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|c| {
            let sig = c
                .get("slot_signature")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !slot_sig.is_empty() && sig == slot_sig {
                return true;
            }
            slot_index.is_some_and(|i| c.get("slot_index").and_then(Value::as_i64) == Some(i))
        })
        .and_then(|c| c.get("cover_id").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    Ok((
        format!("cover na {slot_id} ← shot {shot_id} ({inn}f–{out}f)"),
        cover_id,
    ))
}

/// Human-readable confirm line before committing a cover.
pub fn cover_confirm_summary(marks: &SourceMarks, state: &Value, slot_id: &str) -> String {
    let dur = slot_duration_frames_by_id(state, slot_id).unwrap_or(0);
    let inn = marks.mark_in.unwrap_or(0);
    let out = marks.mark_out.unwrap_or(0);
    let clip = marks.clip_id.as_deref().unwrap_or("?");
    format!("POTVRDI cover · slot={slot_id} · dur={dur}f · IN={inn}f OUT={out}f · clip={clip}")
}
