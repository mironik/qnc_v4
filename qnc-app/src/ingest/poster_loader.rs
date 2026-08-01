//! Async poster fetch for Ingest cards (UI never blocks on HTTP/decode).

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use eframe::egui::{self, ColorImage};

struct PosterRequest {
    clip_id: String,
    url: String,
    repaint: Option<egui::Context>,
}

pub struct PosterResult {
    pub clip_id: String,
    pub image: Result<ColorImage, String>,
}

pub struct AsyncPosterLoader {
    tx: Sender<PosterRequest>,
    rx: Receiver<PosterResult>,
    in_flight: HashSet<String>,
}

impl Default for AsyncPosterLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncPosterLoader {
    pub fn new() -> Self {
        let (tx_req, rx_req) = mpsc::channel::<PosterRequest>();
        let (tx_res, rx_res) = mpsc::channel::<PosterResult>();

        let _ = thread::Builder::new()
            .name("qnc-ingest-poster-loader".into())
            .spawn(move || {
                while let Ok(req) = rx_req.recv() {
                    let image = download_poster(&req.url);
                    let _ = tx_res.send(PosterResult {
                        clip_id: req.clip_id,
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

    pub fn request(
        &mut self,
        clip_id: String,
        url: String,
        repaint: Option<egui::Context>,
    ) -> bool {
        if clip_id.trim().is_empty() || url.trim().is_empty() || self.in_flight.contains(&clip_id) {
            return false;
        }
        if self
            .tx
            .send(PosterRequest {
                clip_id: clip_id.clone(),
                url,
                repaint,
            })
            .is_err()
        {
            return false;
        }
        self.in_flight.insert(clip_id);
        true
    }

    pub fn poll(&mut self) -> Vec<PosterResult> {
        let mut out = Vec::new();
        while let Ok(result) = self.rx.try_recv() {
            self.in_flight.remove(&result.clip_id);
            out.push(result);
        }
        out
    }

    pub fn clear(&mut self) {
        self.in_flight.clear();
        while self.rx.try_recv().is_ok() {}
    }
}

fn download_poster(url: &str) -> Result<ColorImage, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(5))
        .timeout_read(std::time::Duration::from_secs(30))
        .build();
    let mut reader = agent
        .get(url)
        .call()
        .map_err(|e| e.to_string())?
        .into_reader();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut bytes).map_err(|e| e.to_string())?;
    let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, &rgba.into_raw()))
}
