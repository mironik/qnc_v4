//! egui transport: play/pause/seek + frame refresh + rodio + QNC-timeline + focus I/O/M.

use std::time::{Duration, Instant};

use eframe::egui;
use egui::{ColorImage, TextureHandle, TextureOptions};
use serde_json::Value;

use crate::api::{HostClient, TimelineModel};
use crate::audio::AudioEngine;
use crate::editorial::{
    apply_mark_in_fit_frames, clip_id_from_state, cover_confirm_summary, create_cover_from_marks,
    create_segment, first_empty_slot, kind_for_action, slot_duration_frames_by_id,
    source_frame_at_part, SourceMarks,
};
use crate::focus::{
    duration_frames_from_timeline, focus_chain, fps_from_timeline, frame_to_seconds,
    seconds_to_frame, FocusTarget, TimelineFocus,
};
use crate::shortcuts::StoryBindings;
use crate::timeline;

const FRAME_INTERVAL: Duration = Duration::from_millis(200);
const AUDIO_CHUNK_SEC: f64 = 3.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Wrap,
    Source,
}

#[derive(Debug, Clone)]
struct PendingCover {
    slot_id: String,
    summary: String,
}

#[derive(Debug, Clone)]
enum UndoEntry {
    Cover { cover_id: String },
    Part { part_id: String },
}

pub struct TransportApp {
    host: HostClient,
    session_id: String,
    project_id: String,
    virtual_frame: i64,
    playing: bool,
    layer: String,
    buses: String,
    status: String,
    texture: Option<TextureHandle>,
    pending_image: Option<ColorImage>,
    last_frame_at: Instant,
    last_tick: Instant,
    audio: Option<AudioEngine>,
    audio_until_frame: i64,
    stop_on_close: bool,
    timeline: Option<TimelineModel>,
    bindings: StoryBindings,
    marks: SourceMarks,
    story_state: Value,
    focus: TimelineFocus,
    view_mode: ViewMode,
    source_clip_id: Option<String>,
    source_seek_frame: i64,
    pending_cover: Option<PendingCover>,
    undo_stack: Vec<UndoEntry>,
    show_cheatsheet: bool,
}

impl TransportApp {
    pub fn open(
        host: HostClient,
        project_id: String,
        initial_seek: f64,
        with_audio: bool,
        source_clip_id: Option<String>,
    ) -> Result<(), String> {
        let bindings = load_bindings(&host);
        let story_state = host.story_state(&project_id).unwrap_or(Value::Null);
        let resolved_clip = source_clip_id
            .filter(|s| !s.trim().is_empty())
            .or_else(|| clip_id_from_state(&story_state));
        let view_mode = if resolved_clip.is_some() {
            ViewMode::Source
        } else {
            ViewMode::Wrap
        };
        let timeline = match host.timeline_model(&project_id) {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("timeline-model: {e}");
                None
            }
        };
        let initial_frame =
            seconds_to_frame(initial_seek.max(0.0), fps_from_timeline(timeline.as_ref()));
        let start = host.playback_start(&project_id)?;
        if initial_frame > 0 {
            host.playback_seek_frame(&start.session_id, initial_frame)?;
        }
        let state = host.playback_state(&start.session_id)?;
        let buses = start
            .audio_buses
            .iter()
            .map(|b| b.role.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let audio = if with_audio {
            match AudioEngine::new() {
                Ok(engine) => Some(engine),
                Err(e) => {
                    eprintln!("audio disabled: {e}");
                    None
                }
            }
        } else {
            None
        };
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1180.0, 900.0])
                .with_title(format!("QNC Story — {project_id}")),
            ..Default::default()
        };
        let mut app = TransportApp {
            host,
            session_id: state.session_id.clone(),
            project_id,
            virtual_frame: if initial_frame > 0 {
                initial_frame
            } else {
                state.virtual_frame
            },
            playing: false,
            layer: state.active.layer.clone(),
            buses,
            status: format!(
                "Story · {} · {} · {} · {}",
                bindings.chord_hint("toggle_source_wrap"),
                bindings.chord_hint("focus_empty_slot"),
                bindings.chord_hint("overwrite_cover"),
                bindings.chord_hint("undo_last")
            ),
            texture: None,
            pending_image: None,
            last_frame_at: Instant::now() - FRAME_INTERVAL,
            last_tick: Instant::now(),
            audio,
            audio_until_frame: -1,
            stop_on_close: true,
            timeline,
            bindings,
            marks: SourceMarks::default(),
            story_state,
            focus: TimelineFocus::default(),
            view_mode,
            source_clip_id: resolved_clip,
            source_seek_frame: initial_frame,
            pending_cover: None,
            undo_stack: Vec::new(),
            show_cheatsheet: true,
        };
        if let Err(e) = app.refresh_preview() {
            app.status = format!("preview: {e}");
        }
        eframe::run_native("QNC Story", options, Box::new(move |_cc| Ok(Box::new(app))))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn fps(&self) -> f64 {
        fps_from_timeline(self.timeline.as_ref())
    }

    fn duration_frames(&self) -> i64 {
        duration_frames_from_timeline(self.timeline.as_ref())
    }

    fn virtual_sec(&self) -> f64 {
        frame_to_seconds(self.virtual_frame, self.fps())
    }

    fn source_seek_sec(&self) -> f64 {
        frame_to_seconds(self.source_seek_frame, self.fps())
    }

    fn audio_chunk_frames(&self) -> i64 {
        seconds_to_frame(AUDIO_CHUNK_SEC, self.fps()).max(1)
    }

    fn fetch_frame(&mut self) -> Result<(), String> {
        self.host
            .playback_seek_frame(&self.session_id, self.virtual_frame)?;
        let state = self.host.playback_state(&self.session_id)?;
        self.layer = state.active.layer.clone();
        let url = self.host.frame_url_for_frame(&state, self.virtual_frame);
        let bytes = self.host.download_bytes(&url)?;
        let img = image::load_from_memory(&bytes)
            .map_err(|e| e.to_string())?
            .to_rgba8();
        let size = [img.width() as usize, img.height() as usize];
        self.pending_image = Some(ColorImage::from_rgba_unmultiplied(size, &img.into_raw()));
        self.last_frame_at = Instant::now();
        Ok(())
    }

    fn fetch_source_thumb(&mut self) -> Result<(), String> {
        let clip = self
            .source_clip_id
            .as_deref()
            .ok_or_else(|| "nema source clip".to_string())?;
        let url = self
            .host
            .story_thumbnail_url(&self.project_id, clip, self.source_seek_sec());
        let bytes = self.host.download_bytes(&url)?;
        let img = image::load_from_memory(&bytes)
            .map_err(|e| e.to_string())?
            .to_rgba8();
        let size = [img.width() as usize, img.height() as usize];
        self.pending_image = Some(ColorImage::from_rgba_unmultiplied(size, &img.into_raw()));
        self.last_frame_at = Instant::now();
        Ok(())
    }

    fn refresh_preview(&mut self) -> Result<(), String> {
        match self.view_mode {
            ViewMode::Wrap => self.fetch_frame(),
            ViewMode::Source => self.fetch_source_thumb(),
        }
    }

    fn reload_timeline(&mut self) {
        match self.host.timeline_model(&self.project_id) {
            Ok(m) => {
                self.timeline = Some(m);
            }
            Err(e) => self.status = format!("timeline: {e}"),
        }
    }

    fn reload_story_state(&mut self) {
        match self.host.story_state(&self.project_id) {
            Ok(s) => self.story_state = s,
            Err(e) => self.status = format!("state: {e}"),
        }
    }

    fn restart_playback_session(&mut self) {
        let _ = self.host.playback_stop(&self.session_id);
        match self.host.playback_start(&self.project_id) {
            Ok(start) => {
                self.session_id = start.session_id;
                self.buses = start
                    .audio_buses
                    .iter()
                    .map(|b| b.role.as_str())
                    .collect::<Vec<_>>()
                    .join(",");
                let max = self.duration_frames();
                if max > 0 {
                    self.virtual_frame = self.virtual_frame.clamp(0, max);
                }
                if let Err(e) = self.fetch_frame() {
                    self.status = format!("session restart frame: {e}");
                }
            }
            Err(e) => self.status = format!("playback restart: {e}"),
        }
    }

    fn ensure_audio_chunk(&mut self) {
        let Some(engine) = self.audio.as_ref() else {
            return;
        };
        if !self.playing {
            return;
        }
        let margin = (self.fps() / 4.0).round() as i64;
        if self.virtual_frame + margin < self.audio_until_frame && !engine.empty() {
            return;
        }
        let Ok(state) = self.host.playback_state(&self.session_id) else {
            return;
        };
        let chunk_frames = self.audio_chunk_frames();
        let url = self.host.audio_url_for_frames(&state, chunk_frames);
        match self.host.download_bytes(&url) {
            Ok(bytes) => {
                if let Err(e) = engine.append_m4a(bytes) {
                    self.status = format!("audio decode: {e}");
                } else {
                    self.audio_until_frame = self.virtual_frame + chunk_frames;
                    engine.play();
                }
            }
            Err(e) => self.status = format!("audio fetch: {e}"),
        }
    }

    fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
        let _ = self.host.playback_pause(&self.session_id, !playing);
        if let Some(engine) = self.audio.as_ref() {
            if playing {
                engine.play();
                self.audio_until_frame = -1;
            } else {
                engine.pause();
            }
        }
        self.last_tick = Instant::now();
        self.status = if playing {
            "Playing".into()
        } else {
            "Paused".into()
        };
    }

    fn seek_to_frame(&mut self, frame: i64) {
        let max = self.duration_frames();
        self.virtual_frame = if max > 0 {
            frame.clamp(0, max)
        } else {
            frame.max(0)
        };
        if let Some(engine) = self.audio.as_ref() {
            engine.clear();
            if self.playing {
                engine.play();
            } else {
                engine.pause();
            }
        }
        self.audio_until_frame = -1;
        if let Err(e) = self.fetch_frame() {
            self.status = format!("seek: {e}");
        } else {
            self.status = format!(
                "Seek {}f ({:.3}s) · fokus={}",
                self.virtual_frame,
                self.virtual_sec(),
                self.focus.target.label()
            );
        }
    }

    fn source_at_playhead(&self) -> Option<(String, i64)> {
        if self.view_mode == ViewMode::Source {
            if let Some(clip) = self.source_clip_id.clone() {
                return Some((clip, self.source_seek_frame.max(0)));
            }
        }
        if let Some(clip) = self.source_clip_id.clone() {
            // Prefer explicit source dock even in wrap when marking.
            if matches!(self.focus.target, FocusTarget::In | FocusTarget::Out)
                || self.marks.clip_id.as_ref() == Some(&clip)
            {
                return Some((clip, self.source_seek_frame.max(0)));
            }
        }
        if let Ok(state) = self.host.playback_state(&self.session_id) {
            let part_id = state.active.part_id.trim();
            if !part_id.is_empty() {
                if let Some(pair) =
                    source_frame_at_part(&self.story_state, part_id, state.active.local_frame)
                {
                    return Some(pair);
                }
            }
        }
        let clip = clip_id_from_state(&self.story_state)
            .or_else(|| self.marks.clip_id.clone())
            .or_else(|| self.source_clip_id.clone())?;
        let frame = if self.source_clip_id.as_ref() == Some(&clip) {
            self.source_seek_frame
        } else {
            self.virtual_frame
        };
        Some((clip, frame.max(0)))
    }

    fn sync_marks_from_io_pins(&mut self) {
        if let Some(model) = self.timeline.as_ref() {
            for pin in &model.io_pins {
                if pin.kind.eq_ignore_ascii_case("in") && self.marks.mark_in.is_none() {
                    if let Some((clip, _)) = self.source_at_playhead() {
                        self.marks.set_in(&clip, pin.timeline_frame);
                    }
                }
                if pin.kind.eq_ignore_ascii_case("out") && self.marks.mark_out.is_none() {
                    if let Some((clip, _)) = self.source_at_playhead() {
                        self.marks.set_out(&clip, pin.timeline_frame);
                    }
                }
            }
        }
    }

    fn build_focus_chain(&self) -> Vec<FocusTarget> {
        let marker_ids = self
            .timeline
            .as_ref()
            .map(|m| m.markers.iter().map(|p| p.id.clone()).collect::<Vec<_>>())
            .unwrap_or_default();
        let slot_ids = self
            .timeline
            .as_ref()
            .map(|m| {
                m.marker_slots
                    .iter()
                    .map(|s| s.slot_id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        focus_chain(true, true, marker_ids, slot_ids)
    }

    fn on_focus_changed(&mut self) {
        if let FocusTarget::Slot { id } = self.focus.target.clone() {
            match self.host.select_slot(&self.project_id, &id) {
                Ok(s) => {
                    self.story_state = s;
                    let dur = slot_duration_frames_by_id(&self.story_state, &id).unwrap_or(0);
                    self.status = format!(
                        "Fokus slot {id} · trajanje={dur}f ({:.3}s) (bafer=DB)",
                        frame_to_seconds(dur, self.fps())
                    );
                }
                Err(e) => self.status = format!("slot select: {e}"),
            }
        }
    }

    fn apply_focus_empty_slot(&mut self) {
        self.reload_story_state();
        let Some(slot) = first_empty_slot(&self.story_state) else {
            self.status = "Nema praznog M–M slota".into();
            return;
        };
        let id = slot
            .get("slot_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let dur = slot
            .get("duration_frames")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        match self.host.select_slot(&self.project_id, &id) {
            Ok(s) => {
                self.story_state = s;
                self.focus.select_slot(id.clone());
                self.status = format!("Prazni slot {id} · trajanje={dur}f → Shift+I na source");
            }
            Err(e) => self.status = format!("focus_empty_slot: {e}"),
        }
    }

    fn apply_mark_in_fit(&mut self) {
        self.view_mode = ViewMode::Source;
        match self.source_at_playhead() {
            Some((clip, frame)) => {
                self.source_clip_id = Some(clip.clone());
                self.source_seek_frame = frame;
                match apply_mark_in_fit_frames(&mut self.marks, &clip, frame, &self.story_state) {
                    Ok((inn, out)) => {
                        self.focus.select_out();
                        self.status = format!(
                            "IN={inn}f OUT={out}f (slot dur) · {} · zatim {}",
                            self.marks.summary(),
                            self.bindings.chord_hint("overwrite_cover")
                        );
                        let _ = self.refresh_preview();
                    }
                    Err(e) => self.status = e,
                }
            }
            None => {
                self.status = format!(
                    "Shift+I: nema clipa — odaberi kadar ili {} pa source",
                    self.bindings.chord_hint("toggle_source_wrap")
                );
            }
        }
    }

    fn apply_overwrite_cover(&mut self) {
        let slot_id = match &self.focus.target {
            FocusTarget::Slot { id } => Some(id.clone()),
            _ => self
                .story_state
                .get("selected_slot_id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        };
        let Some(slot_id) = slot_id else {
            self.status = format!(
                "Nema slota — {} ili {}",
                self.bindings.chord_hint("focus_empty_slot"),
                self.bindings.chord_hint("step_next_marker_slot")
            );
            return;
        };
        if self.marks.mark_in.is_none() || self.marks.mark_out.is_none() {
            self.status = format!(
                "Cover treba IN/OUT — {} na source",
                self.bindings.chord_hint("mark_in_fit_duration")
            );
            return;
        }

        let summary = cover_confirm_summary(&self.marks, &self.story_state, &slot_id);
        if let Some(pending) = &self.pending_cover {
            if pending.slot_id == slot_id {
                // Second press = confirm
                match create_cover_from_marks(
                    &self.host,
                    &self.project_id,
                    &self.marks,
                    &self.story_state,
                    Some(&slot_id),
                ) {
                    Ok((msg, cover_id)) => {
                        self.pending_cover = None;
                        self.marks = SourceMarks::default();
                        self.focus.clear();
                        if !cover_id.is_empty() {
                            self.undo_stack.push(UndoEntry::Cover { cover_id });
                        }
                        self.reload_story_state();
                        self.reload_timeline();
                        self.restart_playback_session();
                        self.status =
                            format!("{msg} · undo={}", self.bindings.chord_hint("undo_last"));
                    }
                    Err(e) => self.status = format!("overwrite_cover: {e}"),
                }
                return;
            }
        }
        self.pending_cover = Some(PendingCover {
            slot_id,
            summary: summary.clone(),
        });
        self.status = format!(
            "{summary} · ponovi {} za potvrdu",
            self.bindings.chord_hint("overwrite_cover")
        );
    }

    fn apply_undo(&mut self) {
        let Some(entry) = self.undo_stack.pop() else {
            self.status = "Undo stack prazan".into();
            return;
        };
        let result = match entry {
            UndoEntry::Cover { cover_id } => self
                .host
                .delete_cover(&self.project_id, &cover_id)
                .map(|_| format!("Undo cover {cover_id}")),
            UndoEntry::Part { part_id } => self
                .host
                .delete_part(&self.project_id, &part_id)
                .map(|_| format!("Undo part {part_id}")),
        };
        match result {
            Ok(msg) => {
                self.reload_story_state();
                self.reload_timeline();
                self.restart_playback_session();
                self.status = msg;
            }
            Err(e) => self.status = format!("undo: {e}"),
        }
    }

    fn step_marker_slot(&mut self, forward: bool) {
        let slots: Vec<String> = self
            .timeline
            .as_ref()
            .map(|m| m.marker_slots.iter().map(|s| s.slot_id.clone()).collect())
            .unwrap_or_default();
        if slots.is_empty() {
            self.status = "Nema M–M slotova".into();
            return;
        }
        let cur = match &self.focus.target {
            FocusTarget::Slot { id } => slots.iter().position(|s| s == id),
            _ => self
                .story_state
                .get("selected_slot_id")
                .and_then(|v| v.as_str())
                .and_then(|sel| slots.iter().position(|s| s == sel)),
        };
        let idx = match (cur, forward) {
            (Some(i), true) => (i + 1) % slots.len(),
            (Some(i), false) => {
                if i == 0 {
                    slots.len() - 1
                } else {
                    i - 1
                }
            }
            (None, true) => 0,
            (None, false) => slots.len() - 1,
        };
        let id = slots[idx].clone();
        self.focus.select_slot(id);
        self.on_focus_changed();
    }

    fn apply_mark_in(&mut self) {
        match self.source_at_playhead() {
            Some((clip, frame)) => {
                self.marks.set_in(&clip, frame);
                self.focus.select_in();
                self.status = format!("Mark IN {frame}f · {}", self.marks.summary());
            }
            None => {
                self.status = "Mark IN: nema clip konteksta (odaberi kadar / play-clip)".into();
            }
        }
    }

    fn apply_mark_out(&mut self) {
        match self.source_at_playhead() {
            Some((clip, frame)) => {
                self.marks.set_out(&clip, frame);
                self.focus.select_out();
                self.status = format!("Mark OUT {frame}f · {}", self.marks.summary());
            }
            None => {
                self.status = "Mark OUT: nema clip konteksta (odaberi kadar / play-clip)".into();
            }
        }
    }

    fn apply_add_marker(&mut self) {
        let at = self.virtual_frame;
        match self.host.create_marker_frame(&self.project_id, at, None) {
            Ok(_) => {
                self.reload_story_state();
                self.reload_timeline();
                if let Some(model) = self.timeline.as_ref() {
                    if let Some(nearest) = model.markers.iter().min_by(|a, b| {
                        (a.timeline_frame - at)
                            .abs()
                            .cmp(&(b.timeline_frame - at).abs())
                    }) {
                        self.focus.select_marker(nearest.id.clone());
                    }
                }
                self.status = format!("M @ {at}f");
            }
            Err(e) => self.status = format!("add_marker: {e}"),
        }
    }

    fn apply_create_segment(&mut self, action_id: &str) {
        let Some(kind) = kind_for_action(action_id) else {
            return;
        };
        match create_segment(&self.host, &self.project_id, kind, &self.marks) {
            Ok((msg, part_id)) => {
                self.marks = SourceMarks::default();
                self.focus.clear();
                self.pending_cover = None;
                if !part_id.is_empty() {
                    self.undo_stack.push(UndoEntry::Part { part_id });
                }
                self.reload_story_state();
                self.reload_timeline();
                self.restart_playback_session();
                self.status = format!("{msg} · undo={}", self.bindings.chord_hint("undo_last"));
            }
            Err(e) => self.status = format!("{action_id}: {e}"),
        }
    }

    fn ensure_in_selected(&mut self) {
        self.sync_marks_from_io_pins();
        if self.marks.mark_in.is_none() {
            if let Some((clip, frame)) = self.source_at_playhead() {
                self.marks.set_in(&clip, frame);
            }
        }
        self.focus.select_in();
        self.status = format!("Fokus IN · {}", self.marks.summary());
    }

    fn ensure_out_selected(&mut self) {
        self.sync_marks_from_io_pins();
        if self.marks.mark_out.is_none() {
            if let Some((clip, frame)) = self.source_at_playhead() {
                self.marks.set_out(&clip, frame);
            }
        }
        self.focus.select_out();
        self.status = format!("Fokus OUT · {}", self.marks.summary());
    }

    fn select_nearest_or_next_marker(&mut self) {
        let Some(model) = self.timeline.as_ref() else {
            self.status = "Nema timeline markera".into();
            return;
        };
        if model.markers.is_empty() {
            self.status = "Nema M markera — pritisni M za dodavanje".into();
            return;
        }
        if let FocusTarget::Marker { id } = &self.focus.target {
            let ids: Vec<_> = model.markers.iter().map(|m| m.id.as_str()).collect();
            if let Some(i) = ids.iter().position(|x| *x == id.as_str()) {
                let next = ids[(i + 1) % ids.len()].to_string();
                self.focus.select_marker(next.clone());
                self.status = format!("Fokus M:{next}");
                return;
            }
        }
        let t = self.virtual_frame;
        let nearest = model
            .markers
            .iter()
            .min_by(|a, b| {
                (a.timeline_frame - t)
                    .abs()
                    .cmp(&(b.timeline_frame - t).abs())
            })
            .map(|m| m.id.clone());
        if let Some(id) = nearest {
            self.focus.select_marker(id.clone());
            self.status = format!("Fokus M:{id}");
        }
    }

    fn step_focus(&mut self, forward: bool) {
        let delta = if forward { 1 } else { -1 };
        match self.focus.target.clone() {
            FocusTarget::Playhead => {
                if self.view_mode == ViewMode::Source {
                    self.source_seek_frame = (self.source_seek_frame + delta).max(0);
                    if let Err(e) = self.refresh_preview() {
                        self.status = format!("source seek: {e}");
                    } else {
                        self.status = format!(
                            "source {}f ({:.3}s)",
                            self.source_seek_frame,
                            self.source_seek_sec()
                        );
                    }
                } else {
                    self.seek_to_frame(self.virtual_frame + delta);
                }
            }
            FocusTarget::In => {
                let Some(clip) = self
                    .marks
                    .clip_id
                    .clone()
                    .or_else(|| self.source_at_playhead().map(|(c, _)| c))
                else {
                    self.status = "IN nudge: nema clip".into();
                    return;
                };
                let cur = self.marks.mark_in.unwrap_or(self.virtual_frame);
                let next = (cur + delta).max(0);
                self.marks.set_in(&clip, next);
                self.status = format!("IN → {next}f");
            }
            FocusTarget::Out => {
                let Some(clip) = self
                    .marks
                    .clip_id
                    .clone()
                    .or_else(|| self.source_at_playhead().map(|(c, _)| c))
                else {
                    self.status = "OUT nudge: nema clip".into();
                    return;
                };
                let cur = self.marks.mark_out.unwrap_or(self.virtual_frame);
                let next = (cur + delta).max(0);
                if self.marks.mark_in.is_some_and(|i| next <= i) {
                    self.status = "OUT ne smije prijeći ispred IN".into();
                    return;
                }
                self.marks.set_out(&clip, next);
                self.status = format!("OUT → {next}f");
            }
            FocusTarget::Marker { id } => {
                let Some(model) = self.timeline.as_ref() else {
                    return;
                };
                let Some(pin) = model.markers.iter().find(|m| m.id == id) else {
                    self.status = format!("Marker {id} nestao");
                    self.focus.clear();
                    return;
                };
                let next = (pin.timeline_frame + delta).max(0);
                match self.host.update_marker_frame(&self.project_id, &id, next) {
                    Ok(_) => {
                        self.reload_timeline();
                        self.reload_story_state();
                        self.focus.select_marker(id);
                        self.status = format!("M → {next}f");
                    }
                    Err(e) => self.status = format!("marker nudge: {e}"),
                }
            }
            FocusTarget::Slot { id } => {
                // Slot bounds are derived from M markers — nudge start/end markers instead.
                self.status = format!(
                    "Slot {id}: pomakni M markere (←/→ na M) · trajanje={}f",
                    slot_duration_frames_by_id(&self.story_state, &id).unwrap_or(0)
                );
            }
        }
    }

    fn apply_delete_marker(&mut self) {
        let marker_id = match &self.focus.target {
            FocusTarget::Marker { id } => Some(id.clone()),
            _ => self
                .timeline
                .as_ref()
                .and_then(|m| {
                    let t = self.virtual_frame;
                    m.markers.iter().min_by(|a, b| {
                        (a.timeline_frame - t)
                            .abs()
                            .cmp(&(b.timeline_frame - t).abs())
                    })
                })
                .map(|m| m.id.clone()),
        };
        let Some(id) = marker_id else {
            self.status = format!(
                "Nema M markera ({} pa {})",
                self.bindings.chord_hint("select_marker"),
                self.bindings.chord_hint("delete_marker")
            );
            return;
        };
        match self.host.delete_marker(&self.project_id, &id) {
            Ok(_) => {
                self.focus.clear();
                self.reload_story_state();
                self.reload_timeline();
                self.status = format!("Obrisan M:{id}");
            }
            Err(e) => self.status = format!("delete_marker: {e}"),
        }
    }

    fn handle_action(&mut self, action_id: &str) {
        match action_id {
            "play_pause" => self.set_playing(!self.playing),
            "step_back_frame" => self.step_focus(false),
            "step_forward_frame" => self.step_focus(true),
            "mark_in" => self.apply_mark_in(),
            "mark_out" => self.apply_mark_out(),
            "add_marker" | "add_marker_continue" => self.apply_add_marker(),
            "select_mark_in" => self.ensure_in_selected(),
            "select_mark_out" => self.ensure_out_selected(),
            "select_marker" => self.select_nearest_or_next_marker(),
            "delete_marker" => self.apply_delete_marker(),
            "delete_part" => {
                self.status = format!(
                    "delete_part: još nije u native UI · za M: {} pa {}",
                    self.bindings.chord_hint("select_marker"),
                    self.bindings.chord_hint("delete_marker")
                );
            }
            "focus_next" => {
                let chain = self.build_focus_chain();
                self.focus.focus_next(&chain);
                self.on_focus_changed();
                if !matches!(self.focus.target, FocusTarget::Slot { .. }) {
                    self.status = format!("Fokus → {}", self.focus.target.label());
                }
            }
            "focus_prev" => {
                let chain = self.build_focus_chain();
                self.focus.focus_prev(&chain);
                self.on_focus_changed();
                if !matches!(self.focus.target, FocusTarget::Slot { .. }) {
                    self.status = format!("Fokus → {}", self.focus.target.label());
                }
            }
            "focus_empty_slot" => self.apply_focus_empty_slot(),
            "step_next_marker_slot" => self.step_marker_slot(true),
            "step_prev_marker_slot" => self.step_marker_slot(false),
            "mark_in_fit_duration" => self.apply_mark_in_fit(),
            "overwrite_cover" | "quick_overwrite_cover" => self.apply_overwrite_cover(),
            "undo_last" => self.apply_undo(),
            "toggle_cheatsheet" => {
                self.show_cheatsheet = !self.show_cheatsheet;
                self.status = if self.show_cheatsheet {
                    "Cheat-sheet ON".into()
                } else {
                    "Cheat-sheet OFF".into()
                };
            }
            "toggle_source_wrap" => {
                self.view_mode = match self.view_mode {
                    ViewMode::Wrap => ViewMode::Source,
                    ViewMode::Source => ViewMode::Wrap,
                };
                if self.view_mode == ViewMode::Source && self.source_clip_id.is_none() {
                    self.source_clip_id = clip_id_from_state(&self.story_state);
                }
                let _ = self.refresh_preview();
                self.status = format!(
                    "Mode={:?} · clip={}",
                    self.view_mode,
                    self.source_clip_id.as_deref().unwrap_or("—")
                );
            }
            "clear_focus" => {
                self.pending_cover = None;
                if !self.focus.is_playhead() {
                    self.focus.clear();
                    self.status = "Fokus → playhead".into();
                } else {
                    self.status = format!(
                        "Fokus već na playheadu ({})",
                        self.bindings.chord_hint("clear_focus")
                    );
                }
            }
            "close_player" => {
                // Catalog may bind Escape here when focus is already playhead.
                self.status = format!(
                    "close_player ({})",
                    self.bindings.chord_hint("close_player")
                );
            }
            "add_ton_segment" | "add_off_segment" => self.apply_create_segment(action_id),
            _ => {}
        }
    }

    fn consume_keys(&mut self, ctx: &egui::Context) {
        let action = ctx.input(|i| {
            for event in &i.events {
                let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = event
                else {
                    continue;
                };
                if let Some(action) =
                    self.bindings
                        .resolve_action(*key, *modifiers, &self.focus.target)
                {
                    return Some(action.to_string());
                }
            }
            None
        });
        if let Some(action) = action {
            self.handle_action(&action);
        }
    }
}

fn load_bindings(host: &HostClient) -> StoryBindings {
    let catalog = host.keyboard_catalog().unwrap_or(Value::Null);
    let user = host.keyboard_user().unwrap_or(Value::Null);
    StoryBindings::from_catalog(&catalog, &user, "storyboard")
}

impl eframe::App for TransportApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.consume_keys(ctx);

        if self.playing && self.view_mode == ViewMode::Wrap {
            let dt = self.last_tick.elapsed().as_secs_f64();
            self.last_tick = Instant::now();
            let delta_frames = seconds_to_frame(dt, self.fps()).max(1);
            let max = self.duration_frames();
            self.virtual_frame = if max > 0 {
                (self.virtual_frame + delta_frames).clamp(0, max)
            } else {
                (self.virtual_frame + delta_frames).max(0)
            };
            if self.last_frame_at.elapsed() >= FRAME_INTERVAL {
                if let Err(e) = self.fetch_frame() {
                    self.status = format!("frame: {e}");
                }
            }
            self.ensure_audio_chunk();
            ctx.request_repaint_after(Duration::from_millis(33));
        }

        if let Some(img) = self.pending_image.take() {
            match &mut self.texture {
                Some(tex) => tex.set(img, TextureOptions::LINEAR),
                None => {
                    self.texture =
                        Some(ctx.load_texture("preview_frame", img, TextureOptions::LINEAR));
                }
            }
        }

        egui::TopBottomPanel::top("transport").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button(if self.playing { "Pause" } else { "Play" })
                    .clicked()
                {
                    self.set_playing(!self.playing);
                }
                let mode_label = match self.view_mode {
                    ViewMode::Wrap => "Wrap",
                    ViewMode::Source => "Source",
                };
                if ui.button(format!("Mode:{mode_label}")).clicked() {
                    self.handle_action("toggle_source_wrap");
                }
                if ui.button("−1f").clicked() {
                    self.step_focus(false);
                }
                if ui.button("+1f").clicked() {
                    self.step_focus(true);
                }
                if ui.button("IN").clicked() {
                    self.apply_mark_in();
                }
                if ui.button("OUT").clicked() {
                    self.apply_mark_out();
                }
                if ui.button("FitIN").clicked() {
                    self.apply_mark_in_fit();
                }
                if ui.button("M").clicked() {
                    self.apply_add_marker();
                }
                if ui.button("Cover").clicked() {
                    self.apply_overwrite_cover();
                }
                if ui.button("Undo").clicked() {
                    self.apply_undo();
                }
                if ui.button(self.bindings.label("add_ton_segment")).clicked() {
                    self.apply_create_segment("add_ton_segment");
                }
                if ui.button(self.bindings.label("add_off_segment")).clicked() {
                    self.apply_create_segment("add_off_segment");
                }
                if ui.button("Keys").clicked() {
                    self.show_cheatsheet = !self.show_cheatsheet;
                }
                ui.separator();
                ui.label(format!(
                    "wrap={}f ({:.3}s)",
                    self.virtual_frame,
                    self.virtual_sec()
                ));
                ui.label(format!(
                    "src={}f ({:.3}s)",
                    self.source_seek_frame,
                    self.source_seek_sec()
                ));
                ui.label(format!("fokus={}", self.focus.target.label()));
            });
            ui.label(format!(
                "project={} · clip={} · {}",
                self.project_id,
                self.source_clip_id.as_deref().unwrap_or("—"),
                self.status
            ));
            ui.label(self.marks.summary());
            if let Some(p) = &self.pending_cover {
                ui.colored_label(egui::Color32::from_rgb(255, 180, 60), &p.summary);
            }
        });

        if self.show_cheatsheet {
            egui::SidePanel::right("cheatsheet")
                .resizable(true)
                .default_width(260.0)
                .show(ctx, |ui| {
                    ui.heading("Tipke (katalog)");
                    ui.separator();
                    let rows = [
                        "play_pause",
                        "step_back_frame",
                        "step_forward_frame",
                        "toggle_source_wrap",
                        "mark_in",
                        "mark_out",
                        "mark_in_fit_duration",
                        "select_mark_in",
                        "focus_empty_slot",
                        "step_next_marker_slot",
                        "step_prev_marker_slot",
                        "overwrite_cover",
                        "add_ton_segment",
                        "add_off_segment",
                        "add_marker",
                        "delete_marker",
                        "undo_last",
                        "focus_next",
                        "clear_focus",
                        "toggle_cheatsheet",
                    ];
                    for id in rows {
                        ui.horizontal(|ui| {
                            ui.monospace(self.bindings.chord_hint(id));
                            ui.label(self.bindings.label(id));
                        });
                    }
                });
        }

        egui::TopBottomPanel::bottom("qnc_timeline")
            .resizable(false)
            .exact_height(150.0)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Wrap timeline · {} · {} · {}",
                    self.bindings.chord_hint("step_next_marker_slot"),
                    self.bindings.chord_hint("focus_empty_slot"),
                    self.bindings.chord_hint("overwrite_cover")
                ));
                if let Some(model) = self.timeline.clone() {
                    if let Some(t) = timeline::paint_timeline(
                        ui,
                        &model,
                        self.virtual_frame,
                        Some(&self.focus.target),
                    ) {
                        self.seek_to_frame(t);
                    }
                } else {
                    ui.colored_label(
                        egui::Color32::from_rgb(200, 120, 80),
                        "Timeline model nije učitan.",
                    );
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            let title = match self.view_mode {
                ViewMode::Wrap => "Wrap preview",
                ViewMode::Source => "Source preview",
            };
            ui.heading(title);
            if let Some(tex) = &self.texture {
                let max_w = ui.available_width().min(960.0);
                let size = tex.size_vec2();
                let scale = (max_w / size.x).min(1.0);
                ui.image((tex.id(), size * scale));
            } else {
                ui.label("Nema framea / thumba.");
            }
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.stop_on_close {
            let _ = self.host.playback_stop(&self.session_id);
            self.stop_on_close = false;
        }
    }
}
