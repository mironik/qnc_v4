//! HTTP helpers for qnc-host playback API.

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct Health {
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct PlaybackState {
    pub session_id: String,
    #[serde(default)]
    pub virtual_frame: i64,
    pub virtual_sec: f64,
    #[serde(default)]
    pub playing: bool,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub timebase_fps: f64,
    pub active: ActiveLayer,
    #[serde(default)]
    pub audio_buses: Vec<AudioBus>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct ActiveLayer {
    #[serde(default)]
    pub mixed_audio_url: String,
    #[serde(default)]
    pub preview_frame_url: String,
    #[serde(default)]
    pub layer: String,
    #[serde(default)]
    pub part_id: String,
    #[serde(default)]
    pub source_sec: f64,
    #[serde(default)]
    pub local_frame: i64,
    #[serde(default)]
    pub source_frame: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AudioBus {
    pub role: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineApplication {
    Wrap,
    Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentSchema {
    Off,
    Ton,
    Source,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct TimelineCover {
    pub cover_id: String,
    #[serde(default)]
    pub clip_id: String,
    pub timeline_start_sec: f64,
    pub timeline_end_sec: f64,
    #[serde(default)]
    pub timeline_start_frame: i64,
    #[serde(default)]
    pub timeline_end_frame: i64,
    #[serde(default)]
    pub streamable: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct TimelinePin {
    pub id: String,
    #[serde(default)]
    pub kind: String,
    pub timeline_sec: f64,
    #[serde(default)]
    pub timeline_frame: i64,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct TimelineMarkerSlot {
    pub slot_id: String,
    pub start_sec: f64,
    pub end_sec: f64,
    #[serde(default)]
    pub start_frame: i64,
    #[serde(default)]
    pub end_frame: i64,
    #[serde(default)]
    pub duration_sec: f64,
    #[serde(default)]
    pub duration_frames: i64,
    #[serde(default)]
    pub slot_index: i64,
    #[serde(default)]
    pub slot_signature: String,
    #[serde(default)]
    pub start_marker_id: String,
    #[serde(default)]
    pub end_marker_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct TimelineSegment {
    pub part_id: String,
    #[serde(default)]
    pub kind: String,
    pub schema: SegmentSchema,
    #[serde(default)]
    pub clip_id: String,
    pub global_start_sec: f64,
    pub global_end_sec: f64,
    pub duration_sec: f64,
    #[serde(default)]
    pub global_start_frame: i64,
    #[serde(default)]
    pub global_end_frame: i64,
    #[serde(default)]
    pub duration_frames: i64,
    #[serde(default)]
    pub streamable: bool,
    #[serde(default)]
    pub covers: Vec<TimelineCover>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct TimelineModel {
    pub project_id: String,
    #[serde(default = "default_wrap_application")]
    pub application: TimelineApplication,
    #[serde(default)]
    pub timeline_fps: f64,
    pub duration_sec: f64,
    #[serde(default)]
    pub duration_frames: i64,
    #[serde(default)]
    pub rows: Vec<String>,
    #[serde(default)]
    pub segments: Vec<TimelineSegment>,
    #[serde(default)]
    pub io_pins: Vec<TimelinePin>,
    #[serde(default)]
    pub markers: Vec<TimelinePin>,
    #[serde(default)]
    pub marker_slots: Vec<TimelineMarkerSlot>,
}

fn default_wrap_application() -> TimelineApplication {
    TimelineApplication::Wrap
}

#[derive(Clone)]
pub struct HostClient {
    pub base: String,
    agent: ureq::Agent,
}

impl HostClient {
    pub fn new(base: &str) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(3))
            .timeout_read(Duration::from_secs(90))
            .build();
        Self {
            base: base.trim_end_matches('/').to_string(),
            agent,
        }
    }

    pub fn health(&self) -> Result<Health, String> {
        self.agent
            .get(&format!("{}/api/health", self.base))
            .call()
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn playback_start(&self, project_id: &str) -> Result<PlaybackState, String> {
        self.agent
            .post(&format!("{}/api/story/playback/start", self.base))
            .send_json(ureq::json!({ "project_id": project_id }))
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn playback_stop(&self, session_id: &str) -> Result<(), String> {
        let _: Value = self
            .agent
            .post(&format!("{}/api/story/playback/stop", self.base))
            .send_json(ureq::json!({ "session_id": session_id }))
            .map_err(|e| e.to_string())?
            .into_json()
            .unwrap_or(Value::Null);
        Ok(())
    }

    #[deprecated(note = "playback seek je frame-only; use playback_seek_frame")]
    #[allow(dead_code)]
    pub fn playback_seek(&self, session_id: &str, virtual_sec: f64) -> Result<(), String> {
        let _: Value = self
            .agent
            .post(&format!("{}/api/story/playback/seek", self.base))
            .send_json(ureq::json!({
                "session_id": session_id,
                "virtual_sec": virtual_sec.max(0.0)
            }))
            .map_err(|e| e.to_string())?
            .into_json()
            .unwrap_or(Value::Null);
        Ok(())
    }

    pub fn playback_seek_frame(&self, session_id: &str, virtual_frame: i64) -> Result<(), String> {
        let _: Value = self
            .agent
            .post(&format!("{}/api/story/playback/seek", self.base))
            .send_json(ureq::json!({
                "session_id": session_id,
                "virtual_frame": virtual_frame.max(0)
            }))
            .map_err(|e| e.to_string())?
            .into_json()
            .unwrap_or(Value::Null);
        Ok(())
    }

    pub fn playback_pause(&self, session_id: &str, paused: bool) -> Result<(), String> {
        let _: Value = self
            .agent
            .post(&format!("{}/api/story/playback/pause", self.base))
            .send_json(ureq::json!({ "session_id": session_id, "paused": paused }))
            .map_err(|e| e.to_string())?
            .into_json()
            .unwrap_or(Value::Null);
        Ok(())
    }

    pub fn playback_state(&self, session_id: &str) -> Result<PlaybackState, String> {
        self.agent
            .get(&format!(
                "{}/api/story/playback/state?session_id={}",
                self.base,
                urlencoding(session_id)
            ))
            .call()
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn timeline_model(&self, project_id: &str) -> Result<TimelineModel, String> {
        self.agent
            .get(&format!(
                "{}/api/story/timeline-model?project_id={}",
                self.base,
                urlencoding(project_id)
            ))
            .call()
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    #[deprecated(
        note = "source timeline model je frame-only; use source_timeline_model_for_frames"
    )]
    #[allow(dead_code)]
    pub fn source_timeline_model(
        &self,
        project_id: &str,
        clip_id: &str,
        _duration_sec: f64,
        _in_sec: f64,
        _out_sec: f64,
    ) -> Result<TimelineModel, String> {
        let _ = (project_id, clip_id);
        Err("source timeline model je frame-only; frameove izračunaj iz source FPS-a i pozovi source_timeline_model_for_frames".into())
    }

    #[allow(dead_code)]
    pub fn source_timeline_model_for_frames(
        &self,
        project_id: &str,
        clip_id: &str,
        duration_frames: i64,
        in_frame: i64,
        out_frame: i64,
    ) -> Result<TimelineModel, String> {
        self.agent
            .get(&format!(
                "{}/api/story/timeline-model/source?project_id={}&clip_id={}&duration_frames={}&in_frame={}&out_frame={}",
                self.base,
                urlencoding(project_id),
                urlencoding(clip_id),
                duration_frames.max(0),
                in_frame.max(0),
                out_frame.max(in_frame.max(0))
            ))
            .call()
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn absolute(&self, rel: &str) -> String {
        if rel.starts_with("http://") || rel.starts_with("https://") {
            rel.to_string()
        } else if rel.starts_with('/') {
            format!("{}{rel}", self.base)
        } else {
            format!("{}/{rel}", self.base)
        }
    }

    pub fn download_bytes(&self, url: &str) -> Result<Vec<u8>, String> {
        let mut reader = self
            .agent
            .get(url)
            .call()
            .map_err(|e| e.to_string())?
            .into_reader();
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        if buf.len() < 32 {
            return Err(format!("download too small ({} bytes): {url}", buf.len()));
        }
        Ok(buf)
    }

    pub fn download_file(&self, url: &str, dest: &Path) -> Result<(), String> {
        let bytes = self.download_bytes(url)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(dest, bytes).map_err(|e| e.to_string())
    }

    #[deprecated(note = "playback frame render je frame-only; use frame_url_for_frame")]
    #[allow(dead_code)]
    pub fn frame_url(&self, state: &PlaybackState, virtual_sec: f64) -> String {
        let rel = if state.active.preview_frame_url.is_empty() {
            format!(
                "/api/story/playback/frame?session_id={}&virtual_sec={virtual_sec:.3}",
                urlencoding(&state.session_id)
            )
        } else {
            // Host may bake virtual_sec into URL; rebuild from clock for transport.
            format!(
                "/api/story/playback/frame?session_id={}&virtual_sec={virtual_sec:.3}",
                urlencoding(&state.session_id)
            )
        };
        self.absolute(&rel)
    }

    pub fn frame_url_for_frame(&self, state: &PlaybackState, virtual_frame: i64) -> String {
        let rel = format!(
            "/api/story/playback/frame?session_id={}&virtual_frame={}",
            urlencoding(&state.session_id),
            virtual_frame.max(0)
        );
        self.absolute(&rel)
    }

    #[deprecated(note = "playback audio je frame-only; use audio_url_for_frames")]
    #[allow(dead_code)]
    pub fn audio_url(&self, state: &PlaybackState, duration_sec: f64) -> String {
        let rel = format!(
            "/api/story/playback/audio?session_id={}&duration_sec={duration_sec:.3}",
            urlencoding(&state.session_id)
        );
        let _ = &state.active.mixed_audio_url;
        self.absolute(&rel)
    }

    pub fn audio_url_for_frames(&self, state: &PlaybackState, duration_frames: i64) -> String {
        let rel = format!(
            "/api/story/playback/audio?session_id={}&duration_frames={}",
            urlencoding(&state.session_id),
            duration_frames.max(1)
        );
        let _ = &state.active.mixed_audio_url;
        self.absolute(&rel)
    }

    /// Story plugin media (proxy-first) — works on current host product path.
    /// Note: host may return a bounded byte chunk (no trailing moov); prefer
    /// `story_virtual_stream_url` for decodeable short windows.
    #[allow(dead_code)]
    pub fn story_media_url(&self, project_id: &str, clip_id: &str) -> String {
        format!(
            "{}/api/story/media?project_id={}&clip_id={}",
            self.base,
            urlencoding(project_id),
            urlencoding(clip_id)
        )
    }

    pub fn story_thumbnail_url(&self, project_id: &str, clip_id: &str, seek: f64) -> String {
        format!(
            "{}/api/story/thumbnail?project_id={}&clip_id={}&seek={seek:.3}",
            self.base,
            urlencoding(project_id),
            urlencoding(clip_id)
        )
    }

    /// Remuxed short window from project proxy (complete MP4 with moov).
    pub fn story_virtual_stream_url(
        &self,
        project_id: &str,
        clip_id: &str,
        in_sec: f64,
        out_sec: f64,
        audio_only: bool,
    ) -> String {
        let ao = if audio_only { "1" } else { "0" };
        format!(
            "{}/api/story/virtual-stream?project_id={}&clip_id={}&in_seconds={in_sec:.3}&out_seconds={out_sec:.3}&audio_only={ao}",
            self.base,
            urlencoding(project_id),
            urlencoding(clip_id)
        )
    }

    /// Full keyboard catalog (manifest). Served as static shell asset.
    pub fn keyboard_catalog(&self) -> Result<Value, String> {
        self.agent
            .get(&format!("{}/api/shell/keyboard-shortcuts", self.base))
            .call()
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn keyboard_user(&self) -> Result<Value, String> {
        self.agent
            .get(&format!("{}/api/settings/keyboard-shortcuts", self.base))
            .call()
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn story_state(&self, project_id: &str) -> Result<Value, String> {
        self.agent
            .get(&format!(
                "{}/api/story/state?project_id={}",
                self.base,
                urlencoding(project_id)
            ))
            .call()
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn shot_select(&self, project_id: &str, virtual_shot_id: &str) -> Result<Value, String> {
        self.agent
            .post(&format!("{}/api/story/shot/select", self.base))
            .send_json(ureq::json!({
                "project_id": project_id,
                "virtual_shot_id": virtual_shot_id,
            }))
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn create_part(
        &self,
        project_id: &str,
        kind: &str,
        virtual_shot_id: Option<&str>,
    ) -> Result<Value, String> {
        self.create_part_ex(project_id, kind, virtual_shot_id, None, None, None)
    }

    /// Virtual segment from source IN/OUT for the Segment tab.
    pub fn create_part_ex(
        &self,
        project_id: &str,
        kind: &str,
        virtual_shot_id: Option<&str>,
        clip_id: Option<&str>,
        in_seconds: Option<f64>,
        out_seconds: Option<f64>,
    ) -> Result<Value, String> {
        let mut body = ureq::json!({
            "project_id": project_id,
            "kind": kind,
        });
        if let Some(id) = virtual_shot_id.filter(|s| !s.is_empty()) {
            body["virtual_shot_id"] = Value::String(id.to_string());
        }
        if let Some(id) = clip_id.filter(|s| !s.is_empty()) {
            body["clip_id"] = Value::String(id.to_string());
        }
        if let Some(v) = in_seconds {
            body["in_seconds"] = Value::from(v);
        }
        if let Some(v) = out_seconds {
            body["out_seconds"] = Value::from(v);
        }
        self.agent
            .post(&format!("{}/api/story/part/create", self.base))
            .send_json(body)
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    /// Derive a source trim as a virtual shot (IN/OUT in source seconds).
    #[allow(dead_code)]
    pub fn create_virtual_shot(
        &self,
        project_id: &str,
        clip_id: &str,
        in_seconds: f64,
        out_seconds: f64,
    ) -> Result<Value, String> {
        self.agent
            .post(&format!("{}/api/story/virtual-shot", self.base))
            .send_json(ureq::json!({
                "project_id": project_id,
                "clip_id": clip_id,
                "in_seconds": in_seconds.max(0.0),
                "out_seconds": out_seconds.max(in_seconds + 0.04),
            }))
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn create_part_ex_frames(
        &self,
        project_id: &str,
        kind: &str,
        virtual_shot_id: Option<&str>,
        clip_id: Option<&str>,
        in_frame: Option<i64>,
        out_frame: Option<i64>,
    ) -> Result<Value, String> {
        let mut body = ureq::json!({
            "project_id": project_id,
            "kind": kind,
        });
        if let Some(id) = virtual_shot_id.filter(|s| !s.is_empty()) {
            body["virtual_shot_id"] = Value::String(id.to_string());
        }
        if let Some(id) = clip_id.filter(|s| !s.is_empty()) {
            body["clip_id"] = Value::String(id.to_string());
        }
        let in_base = in_frame.unwrap_or(0).max(0);
        if in_frame.is_some() {
            body["in_frame"] = Value::from(in_base);
        }
        if let Some(v) = out_frame {
            body["out_frame"] = Value::from(v.max(in_base + 1));
        }
        self.agent
            .post(&format!("{}/api/story/part/create", self.base))
            .send_json(body)
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn create_virtual_shot_from_frames(
        &self,
        project_id: &str,
        clip_id: &str,
        in_frame: i64,
        out_frame: i64,
    ) -> Result<Value, String> {
        self.agent
            .post(&format!("{}/api/story/virtual-shot", self.base))
            .send_json(ureq::json!({
                "project_id": project_id,
                "clip_id": clip_id,
                "in_frame": in_frame.max(0),
                "out_frame": out_frame.max(in_frame + 1),
            }))
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    #[allow(dead_code)]
    pub fn create_marker(
        &self,
        project_id: &str,
        timeline_sec: f64,
        part_id: Option<&str>,
    ) -> Result<Value, String> {
        let mut body = ureq::json!({
            "project_id": project_id,
            "timeline_sec": timeline_sec.max(0.0),
        });
        if let Some(id) = part_id.filter(|s| !s.is_empty()) {
            body["part_id"] = Value::String(id.to_string());
        }
        self.agent
            .post(&format!("{}/api/story/marker/create", self.base))
            .send_json(body)
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn create_marker_frame(
        &self,
        project_id: &str,
        timeline_frame: i64,
        part_id: Option<&str>,
    ) -> Result<Value, String> {
        let mut body = ureq::json!({
            "project_id": project_id,
            "timeline_frame": timeline_frame.max(0),
        });
        if let Some(id) = part_id.filter(|s| !s.is_empty()) {
            body["part_id"] = Value::String(id.to_string());
        }
        self.agent
            .post(&format!("{}/api/story/marker/create", self.base))
            .send_json(body)
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    #[allow(dead_code)]
    pub fn update_marker(
        &self,
        project_id: &str,
        marker_id: &str,
        timeline_sec: f64,
    ) -> Result<Value, String> {
        self.agent
            .post(&format!("{}/api/story/marker/update", self.base))
            .send_json(ureq::json!({
                "project_id": project_id,
                "marker_id": marker_id,
                "timeline_sec": timeline_sec.max(0.0),
            }))
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn update_marker_frame(
        &self,
        project_id: &str,
        marker_id: &str,
        timeline_frame: i64,
    ) -> Result<Value, String> {
        self.agent
            .post(&format!("{}/api/story/marker/update", self.base))
            .send_json(ureq::json!({
                "project_id": project_id,
                "marker_id": marker_id,
                "timeline_frame": timeline_frame.max(0),
            }))
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn delete_marker(&self, project_id: &str, marker_id: &str) -> Result<Value, String> {
        self.agent
            .post(&format!("{}/api/story/marker/delete", self.base))
            .send_json(ureq::json!({
                "project_id": project_id,
                "marker_id": marker_id,
            }))
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn select_slot(&self, project_id: &str, slot_id: &str) -> Result<Value, String> {
        self.agent
            .post(&format!("{}/api/story/marker_slot/select", self.base))
            .send_json(ureq::json!({
                "project_id": project_id,
                "slot_id": slot_id,
            }))
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn create_cover(
        &self,
        project_id: &str,
        slot_id: &str,
        virtual_shot_id: Option<&str>,
        clip_id: Option<&str>,
    ) -> Result<Value, String> {
        let mut body = ureq::json!({
            "project_id": project_id,
            "slot_id": slot_id,
        });
        if let Some(id) = virtual_shot_id.filter(|s| !s.is_empty()) {
            body["virtual_shot_id"] = Value::String(id.to_string());
        }
        if let Some(id) = clip_id.filter(|s| !s.is_empty()) {
            body["clip_id"] = Value::String(id.to_string());
        }
        self.agent
            .post(&format!("{}/api/story/cover/create", self.base))
            .send_json(body)
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn delete_cover(&self, project_id: &str, cover_id: &str) -> Result<Value, String> {
        self.agent
            .post(&format!("{}/api/story/cover/delete", self.base))
            .send_json(ureq::json!({
                "project_id": project_id,
                "cover_id": cover_id,
            }))
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn delete_part(&self, project_id: &str, part_id: &str) -> Result<Value, String> {
        self.agent
            .post(&format!("{}/api/story/part/delete", self.base))
            .send_json(ureq::json!({
                "project_id": project_id,
                "part_id": part_id,
            }))
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }
}

pub fn urlencoding(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
