//! Play a single imported proxy clip via /api/story (no editorial parts required).
//! F5.3+: I/O/T/V + focus (Tab / Ctrl+I/O) · ←/→ = 1 frame on focus.

use std::time::Duration;

use eframe::egui;
use egui::{ColorImage, TextureHandle, TextureOptions};
use serde_json::Value;

use crate::api::HostClient;
use crate::audio::AudioEngine;
use crate::editorial::{
    apply_mark_in_fit_frames, create_cover_from_marks, create_segment, kind_for_action, SourceMarks,
};
use crate::focus::{
    frame_to_seconds, is_valid_fps, seconds_to_frame, source_focus_chain, FocusTarget,
    TimelineFocus,
};
use crate::shortcuts::StoryBindings;

const AUDIO_WINDOW_SEC: f64 = 6.0;

#[allow(dead_code)]
pub struct ClipPlayApp {
    host: HostClient,
    project_id: String,
    clip_id: String,
    seek_frame: i64,
    status: String,
    texture: Option<TextureHandle>,
    pending_image: Option<ColorImage>,
    audio: Option<AudioEngine>,
    playing: bool,
    bindings: StoryBindings,
    marks: SourceMarks,
    focus: TimelineFocus,
    fps: f64,
    story_state: Value,
}

#[allow(dead_code)]
impl ClipPlayApp {
    pub fn open(
        host: HostClient,
        project_id: String,
        clip_id: String,
        seek: f64,
        with_audio: bool,
    ) -> Result<(), String> {
        let _ = host.health()?;
        let catalog = host.keyboard_catalog().unwrap_or(Value::Null);
        let user = host.keyboard_user().unwrap_or(Value::Null);
        let bindings = StoryBindings::from_catalog(&catalog, &user, "storyboard");
        let audio = if with_audio {
            match AudioEngine::new() {
                Ok(e) => Some(e),
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
                .with_inner_size([960.0, 720.0])
                .with_title(format!("QNC clip — {clip_id}")),
            ..Default::default()
        };
        let story_state = host.story_state(&project_id).unwrap_or(Value::Null);
        let fps = source_clip_fps(&story_state, &clip_id).ok_or_else(|| {
            format!("Source FPS nije potvrđen za '{clip_id}'; probe metadata mora biti u SQLite.")
        })?;
        let status = format!(
            "Ready — {} · {} · {}",
            bindings.chord_hint("mark_in_fit_duration"),
            bindings.chord_hint("overwrite_cover"),
            bindings.chord_hint("focus_next")
        );
        let mut app = ClipPlayApp {
            host,
            project_id,
            clip_id,
            seek_frame: seconds_to_frame(seek.max(0.0), fps),
            status,
            texture: None,
            pending_image: None,
            audio,
            playing: false,
            bindings,
            marks: SourceMarks::default(),
            focus: TimelineFocus::default(),
            fps,
            story_state,
        };
        if let Err(e) = app.fetch_thumb() {
            app.status = format!("thumb: {e}");
        }
        eframe::run_native(
            "QNC clip play",
            options,
            Box::new(move |_cc| Ok(Box::new(app))),
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn seek_sec(&self) -> f64 {
        frame_to_seconds(self.seek_frame, self.fps)
    }

    fn fetch_thumb(&mut self) -> Result<(), String> {
        let url = self
            .host
            .story_thumbnail_url(&self.project_id, &self.clip_id, self.seek_sec());
        let bytes = self.host.download_bytes(&url)?;
        let img = image::load_from_memory(&bytes)
            .map_err(|e| e.to_string())?
            .to_rgba8();
        let size = [img.width() as usize, img.height() as usize];
        self.pending_image = Some(ColorImage::from_rgba_unmultiplied(size, &img.into_raw()));
        Ok(())
    }

    fn play_audio_window(&mut self) -> Result<(), String> {
        let Some(engine) = self.audio.as_ref() else {
            return Err("audio engine nije dostupan".into());
        };
        let in_sec = self.seek_sec();
        let out_sec = frame_to_seconds(
            self.seek_frame + seconds_to_frame(AUDIO_WINDOW_SEC, self.fps),
            self.fps,
        );
        let url = self.host.story_virtual_stream_url(
            &self.project_id,
            &self.clip_id,
            in_sec,
            out_sec,
            true,
        );
        self.status = format!("Fetching audio {in_sec:.1}–{out_sec:.1}s…");
        let bytes = self.host.download_bytes(&url)?;
        engine.clear();
        engine.append_m4a(bytes)?;
        engine.play();
        self.status = format!("Playing audio {in_sec:.1}–{out_sec:.1}s");
        Ok(())
    }

    fn set_playing(&mut self, playing: bool) {
        if playing {
            if let Err(e) = self.play_audio_window() {
                self.status = e;
                self.playing = false;
                return;
            }
            self.playing = true;
        } else {
            if let Some(engine) = self.audio.as_ref() {
                engine.pause();
            }
            self.playing = false;
            self.status = "Paused".into();
        }
    }

    fn seek_to_frame(&mut self, frame: i64) {
        self.seek_frame = frame.max(0);
        if self.playing {
            let _ = self.play_audio_window();
        }
        if let Err(e) = self.fetch_thumb() {
            self.status = format!("thumb: {e}");
        } else {
            self.status = format!(
                "seek {}f ({:.3}s) · fokus={} · {}",
                self.seek_frame,
                self.seek_sec(),
                self.focus.target.label(),
                self.marks.summary()
            );
        }
    }

    fn apply_mark_in(&mut self) {
        self.marks.set_in(&self.clip_id, self.seek_frame);
        self.focus.select_in();
        self.status = format!("Mark IN {}f · {}", self.seek_frame, self.marks.summary());
    }

    fn apply_mark_out(&mut self) {
        self.marks.set_out(&self.clip_id, self.seek_frame);
        self.focus.select_out();
        self.status = format!("Mark OUT {}f · {}", self.seek_frame, self.marks.summary());
    }

    fn apply_create_segment(&mut self, action_id: &str) {
        let Some(kind) = kind_for_action(action_id) else {
            return;
        };
        if self.marks.clip_id.is_none() {
            self.marks.clip_id = Some(self.clip_id.clone());
        }
        match create_segment(&self.host, &self.project_id, kind, &self.marks) {
            Ok((msg, _)) => {
                self.marks = SourceMarks::default();
                self.focus.clear();
                self.status = format!("{msg} · otvori: play --gui");
            }
            Err(e) => self.status = format!("{action_id}: {e}"),
        }
    }

    fn step_focus(&mut self, forward: bool) {
        let delta = if forward { 1 } else { -1 };
        match self.focus.target.clone() {
            FocusTarget::Playhead | FocusTarget::Marker { .. } => {
                // Source clip preview: no M pins here — marker focus falls back to playhead.
                if matches!(self.focus.target, FocusTarget::Marker { .. }) {
                    self.focus.clear();
                }
                self.seek_to_frame(self.seek_frame + delta);
            }
            FocusTarget::In => {
                let cur = self.marks.mark_in.unwrap_or(self.seek_frame);
                let next = (cur + delta).max(0);
                self.marks.set_in(&self.clip_id, next);
                self.status = format!("IN → {next}f");
            }
            FocusTarget::Out => {
                let cur = self.marks.mark_out.unwrap_or(self.seek_frame);
                let next = (cur + delta).max(0);
                if self.marks.mark_in.is_some_and(|i| next <= i) {
                    self.status = "OUT ne smije prijeći ispred IN".into();
                    return;
                }
                self.marks.set_out(&self.clip_id, next);
                self.status = format!("OUT → {next}f");
            }
            FocusTarget::Slot { .. } => {
                self.status = "Slot fokus: wrap play (play --gui)".into();
            }
        }
    }

    fn handle_action(&mut self, action_id: &str) {
        match action_id {
            "play_pause" => self.set_playing(!self.playing),
            "step_back_frame" => self.step_focus(false),
            "step_forward_frame" => self.step_focus(true),
            "mark_in" => self.apply_mark_in(),
            "mark_out" => self.apply_mark_out(),
            "mark_in_fit_duration" => {
                if let Ok(s) = self.host.story_state(&self.project_id) {
                    self.story_state = s;
                }
                match apply_mark_in_fit_frames(
                    &mut self.marks,
                    &self.clip_id,
                    self.seek_frame,
                    &self.story_state,
                ) {
                    Ok((inn, out)) => {
                        self.focus.select_out();
                        self.status =
                            format!("IN={inn}f OUT={out}f (slot) · {}", self.marks.summary());
                    }
                    Err(e) => self.status = e,
                }
            }
            "overwrite_cover" | "quick_overwrite_cover" => {
                if let Ok(s) = self.host.story_state(&self.project_id) {
                    self.story_state = s;
                }
                match create_cover_from_marks(
                    &self.host,
                    &self.project_id,
                    &self.marks,
                    &self.story_state,
                    None,
                ) {
                    Ok((msg, _)) => {
                        self.marks = SourceMarks::default();
                        self.focus.clear();
                        self.status = msg;
                    }
                    Err(e) => self.status = format!("overwrite_cover: {e}"),
                }
            }
            "select_mark_in" => {
                if self.marks.mark_in.is_none() {
                    self.marks.set_in(&self.clip_id, self.seek_frame);
                }
                self.focus.select_in();
                self.status = format!("Fokus IN · {}", self.marks.summary());
            }
            "select_mark_out" => {
                if self.marks.mark_out.is_none() {
                    self.marks.set_out(&self.clip_id, self.seek_frame);
                }
                self.focus.select_out();
                self.status = format!("Fokus OUT · {}", self.marks.summary());
            }
            "select_marker" => {
                self.status = "M fokus: koristi wrap play (play --gui)".into();
            }
            "focus_next" => {
                let chain = source_focus_chain(std::iter::empty());
                self.focus.focus_next(&chain);
                self.status = format!("Fokus → {}", self.focus.target.label());
            }
            "focus_prev" => {
                let chain = source_focus_chain(std::iter::empty());
                self.focus.focus_prev(&chain);
                self.status = format!("Fokus → {}", self.focus.target.label());
            }
            "clear_focus" => {
                if !self.focus.is_playhead() {
                    self.focus.clear();
                    self.status = "Fokus → playhead".into();
                }
            }
            "close_player" => {}
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

impl eframe::App for ClipPlayApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.consume_keys(ctx);

        if self.playing {
            ctx.request_repaint_after(Duration::from_millis(100));
            if let Some(engine) = self.audio.as_ref() {
                if engine.empty() {
                    self.playing = false;
                    self.status = "Window ended".into();
                }
            }
        }
        if let Some(img) = self.pending_image.take() {
            match &mut self.texture {
                Some(tex) => tex.set(img, TextureOptions::LINEAR),
                None => {
                    self.texture =
                        Some(ctx.load_texture("clip_thumb", img, TextureOptions::LINEAR));
                }
            }
        }

        egui::TopBottomPanel::top("clip_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button(if self.playing { "Pause" } else { "Play" })
                    .clicked()
                {
                    self.set_playing(!self.playing);
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
                if ui.button(self.bindings.label("add_ton_segment")).clicked() {
                    self.apply_create_segment("add_ton_segment");
                }
                if ui.button(self.bindings.label("add_off_segment")).clicked() {
                    self.apply_create_segment("add_off_segment");
                }
                ui.separator();
                ui.label(format!("clip={}", self.clip_id));
                ui.label(format!("t={}f ({:.3}s)", self.seek_frame, self.seek_sec()));
                ui.label(format!("fokus={}", self.focus.target.label()));
            });
            ui.label(format!("project={} · {}", self.project_id, self.status));
            ui.label(self.marks.summary());
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("QNC proxy clip preview");
            if let Some(tex) = &self.texture {
                let max_w = ui.available_width().min(900.0);
                let size = tex.size_vec2();
                let scale = (max_w / size.x).min(1.0);
                ui.image((tex.id(), size * scale));
            } else {
                ui.label("Nema thumbnails.");
            }
            ui.separator();
            ui.label(format!(
                "{} · {} · {} · {} → DB",
                self.bindings.chord_hint("focus_next"),
                self.bindings.chord_hint("select_mark_in"),
                self.bindings.chord_hint("step_forward_frame"),
                self.bindings.chord_hint("add_ton_segment")
            ));
        });
    }
}

/// One-shot CLI: thumb + optional rodio of a remuxed audio window.
pub fn run_oneshot(
    host: &HostClient,
    project_id: &str,
    clip_id: &str,
    seek: f64,
    with_audio: bool,
) -> Result<(), String> {
    let _ = host.health()?;
    let tmp = std::env::temp_dir().join("qnc-client");
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let seek = seek.max(0.0);
    let thumb = tmp.join(format!("thumb_{clip_id}_{seek:.0}.jpg"));
    let thumb_url = host.story_thumbnail_url(project_id, clip_id, seek);
    host.download_file(&thumb_url, &thumb)?;
    println!("thumb={}", thumb.display());
    if with_audio {
        let in_sec = seek;
        let out_sec = seek + AUDIO_WINDOW_SEC;
        let url = host.story_virtual_stream_url(project_id, clip_id, in_sec, out_sec, true);
        let media_path = tmp.join(format!("vs_a_{clip_id}.mp4"));
        println!("Fetching audio window {in_sec:.1}–{out_sec:.1}s…");
        host.download_file(&url, &media_path)?;
        println!("media={}", media_path.display());
        let engine = AudioEngine::new()?;
        let bytes = std::fs::read(&media_path).map_err(|e| e.to_string())?;
        engine.append_m4a(bytes)?;
        engine.play();
        println!("rodio: playing ~{AUDIO_WINDOW_SEC}s…");
        std::thread::sleep(Duration::from_secs_f64(AUDIO_WINDOW_SEC + 0.5));
    }
    Ok(())
}

fn source_clip_fps(story_state: &Value, clip_id: &str) -> Option<f64> {
    story_state
        .get("all_clips")
        .and_then(Value::as_array)
        .and_then(|clips| {
            clips
                .iter()
                .find(|clip| clip.get("clip_id").and_then(Value::as_str) == Some(clip_id))
        })
        .and_then(|clip| {
            clip.get("fps")
                .or_else(|| clip.get("source_fps"))
                .and_then(Value::as_f64)
        })
        .filter(|fps| is_valid_fps(*fps))
}
