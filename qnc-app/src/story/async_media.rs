//! Background media jobs for native Story (thumbs / filmstrip / waveform).
//!
//! Preview decode is owned by the broadcast player (`PlayerRemote`). This module
//! only loads editorial UI assets (thumbnails, filmstrip, waveforms).

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use eframe::egui::{self, ColorImage};

use crate::api::HostClient;

use super::playback_runtime;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum ImageKey {
    Thumb { clip_id: String },
    Film { clip_id: String, index: i64 },
}

pub(super) struct ImageResult {
    pub key: ImageKey,
    pub image: Result<ColorImage, String>,
}

struct ImageRequest {
    key: ImageKey,
    url: String,
    repaint: Option<egui::Context>,
}

pub(super) struct AsyncImageLoader {
    tx: Sender<ImageRequest>,
    rx: Receiver<ImageResult>,
    in_flight: HashSet<ImageKey>,
}

impl AsyncImageLoader {
    pub fn new() -> Self {
        let (tx_req, rx_req) = mpsc::channel::<ImageRequest>();
        let (tx_res, rx_res) = mpsc::channel::<ImageResult>();

        let _ = thread::Builder::new()
            .name("qnc-story-image-loader".into())
            .spawn(move || {
                while let Ok(req) = rx_req.recv() {
                    let image = playback_runtime::download_color_image_url(&req.url);
                    let _ = tx_res.send(ImageResult {
                        key: req.key,
                        image,
                    });
                    if let Some(ctx) = req.repaint {
                        ctx.request_repaint();
                    }
                }
            });

        Self {
            tx: tx_req,
            rx: rx_res,
            in_flight: HashSet::new(),
        }
    }

    pub fn request(&mut self, key: ImageKey, url: String, repaint: Option<egui::Context>) -> bool {
        if url.trim().is_empty() || self.in_flight.contains(&key) {
            return false;
        }
        if self
            .tx
            .send(ImageRequest {
                key: key.clone(),
                url,
                repaint,
            })
            .is_err()
        {
            return false;
        }
        self.in_flight.insert(key);
        true
    }

    pub fn poll(&mut self) -> Vec<ImageResult> {
        let mut out = Vec::new();
        while let Ok(result) = self.rx.try_recv() {
            self.in_flight.remove(&result.key);
            out.push(result);
        }
        out
    }
}

pub(super) struct SourceMedia {
    pub clip_id: String,
    pub a1_peaks: Vec<f32>,
    pub a2_peaks: Vec<f32>,
    pub film_frames: Vec<(i64, f64, String)>,
}

pub(super) struct SourceMediaResult {
    pub clip_id: String,
    pub media: Result<SourceMedia, String>,
}

pub(super) struct AsyncSourceMediaLoader {
    tx: Sender<SourceMediaResult>,
    rx: Receiver<SourceMediaResult>,
    busy_clip: Option<String>,
}

impl AsyncSourceMediaLoader {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            busy_clip: None,
        }
    }

    pub fn request(
        &mut self,
        host: &HostClient,
        project_id: String,
        clip_id: String,
        repaint: Option<egui::Context>,
    ) -> bool {
        if clip_id.trim().is_empty() || self.busy_clip.is_some() {
            return false;
        }
        let tx = self.tx.clone();
        let host = host.clone();
        let clip_for_thread = clip_id.clone();
        let spawn = thread::Builder::new()
            .name("qnc-story-source-media".into())
            .spawn(move || {
                let _ = host.story_timeline_build(&project_id, &clip_for_thread);
                let a1_peaks = host
                    .waveform_peaks(&project_id, &clip_for_thread, 1)
                    .unwrap_or_default();
                let a2_peaks = host
                    .waveform_peaks(&project_id, &clip_for_thread, 2)
                    .unwrap_or_default();
                let film_frames = host
                    .story_filmstrip_frames(&project_id, &clip_for_thread)
                    .unwrap_or_default();
                let _ = tx.send(SourceMediaResult {
                    clip_id: clip_for_thread.clone(),
                    media: Ok(SourceMedia {
                        clip_id: clip_for_thread,
                        a1_peaks,
                        a2_peaks,
                        film_frames,
                    }),
                });
                if let Some(ctx) = repaint {
                    ctx.request_repaint();
                }
            });
        if spawn.is_err() {
            return false;
        }
        self.busy_clip = Some(clip_id);
        true
    }

    pub fn poll(&mut self) -> Vec<SourceMediaResult> {
        let mut out = Vec::new();
        while let Ok(result) = self.rx.try_recv() {
            self.busy_clip = None;
            out.push(result);
        }
        out
    }
}
