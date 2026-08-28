//! Neutral async UI media assets.
//!
//! These loaders are not playback. They fetch thumbnails, poster images,
//! filmstrip JPEGs, waveform peaks, and filmstrip manifests for passive UI
//! projection. Decode/timebase remains owned by the broadcast player.

use std::collections::HashSet;
use std::io::Read;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

use eframe::egui::{self, ColorImage};
use serde_json::json;

use crate::api::{self, HostClient, HostRequestMethod, HostRequestTimeout};

const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(12);
pub(crate) const IMAGE_ASSET_MAX_IN_FLIGHT: usize = 64;
pub(crate) const SOURCE_FILMSTRIP_FRAME_COUNT: usize = 13;
const IMAGE_ASSET_WORKERS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ImageAssetKey {
    pub scope: String,
    pub item_id: String,
    pub variant: String,
}

impl ImageAssetKey {
    pub fn new(
        scope: impl Into<String>,
        item_id: impl Into<String>,
        variant: impl Into<String>,
    ) -> Self {
        Self {
            scope: scope.into(),
            item_id: item_id.into(),
            variant: variant.into(),
        }
    }
}

pub(crate) struct ImageAssetResult {
    pub key: ImageAssetKey,
    pub image: Result<ColorImage, String>,
}

struct ImageAssetRequest {
    key: ImageAssetKey,
    url: String,
    generation: u64,
    repaint: Option<egui::Context>,
}

pub(crate) struct AsyncImageAssetLoader {
    tx: Sender<ImageAssetRequest>,
    rx: Receiver<ImageAssetResult>,
    in_flight: HashSet<ImageAssetKey>,
    generation: Arc<AtomicU64>,
}

impl Default for AsyncImageAssetLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncImageAssetLoader {
    pub fn new() -> Self {
        let (tx_req, rx_req) = mpsc::channel::<ImageAssetRequest>();
        let (tx_res, rx_res) = mpsc::channel::<ImageAssetResult>();
        let generation = Arc::new(AtomicU64::new(0));
        let rx_req = Arc::new(Mutex::new(rx_req));

        for worker_ix in 0..IMAGE_ASSET_WORKERS {
            let worker_generation = generation.clone();
            let worker_rx = rx_req.clone();
            let worker_tx = tx_res.clone();
            let _ = thread::Builder::new()
                .name(format!("qnc-media-image-assets-{worker_ix}"))
                .spawn(move || loop {
                    let req = match worker_rx.lock().expect("image asset queue").recv() {
                        Ok(req) => req,
                        Err(_) => break,
                    };
                    if req.generation != worker_generation.load(Ordering::Acquire) {
                        continue;
                    }
                    let image = download_color_image_url(&req.url);
                    if req.generation != worker_generation.load(Ordering::Acquire) {
                        continue;
                    }
                    let _ = worker_tx.send(ImageAssetResult {
                        key: req.key,
                        image,
                    });
                    if let Some(ctx) = req.repaint {
                        ctx.request_repaint();
                    }
                });
        }

        Self {
            tx: tx_req,
            rx: rx_res,
            in_flight: HashSet::new(),
            generation,
        }
    }

    pub fn request(
        &mut self,
        key: ImageAssetKey,
        url: String,
        repaint: Option<egui::Context>,
    ) -> bool {
        if url.trim().is_empty() || self.in_flight.contains(&key) {
            return false;
        }
        if self.is_saturated() {
            return false;
        }
        if self
            .tx
            .send(ImageAssetRequest {
                key: key.clone(),
                url,
                generation: self.generation.load(Ordering::Acquire),
                repaint,
            })
            .is_err()
        {
            return false;
        }
        self.in_flight.insert(key);
        true
    }

    pub fn is_saturated(&self) -> bool {
        self.in_flight.len() >= IMAGE_ASSET_MAX_IN_FLIGHT
    }

    #[cfg(test)]
    pub fn in_flight_len(&self) -> usize {
        self.in_flight.len()
    }

    pub fn poll(&mut self) -> Vec<ImageAssetResult> {
        let mut out = Vec::new();
        while let Ok(result) = self.rx.try_recv() {
            self.in_flight.remove(&result.key);
            out.push(result);
        }
        out
    }

    pub fn cancel_pending(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.in_flight.clear();
        while self.rx.try_recv().is_ok() {}
    }

    pub fn clear(&mut self) {
        self.cancel_pending();
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SourceFilmFrameAsset {
    pub index: i64,
    pub seek_sec: f64,
    pub url: String,
}

#[derive(Debug, Clone)]
pub(crate) struct SourceMediaAsset {
    pub clip_id: String,
    pub a1_peaks: Vec<f32>,
    pub a2_peaks: Vec<f32>,
    pub waveform_loaded: bool,
    pub film_frames: Vec<SourceFilmFrameAsset>,
    pub filmstrip_ready: bool,
    pub filmstrip_status: String,
    pub filmstrip_error: String,
}

pub(crate) struct SourceMediaAssetResult {
    pub project_id: String,
    pub clip_id: String,
    pub media: Result<SourceMediaAsset, String>,
}

struct SourceMediaAssetRequest {
    host: HostClient,
    project_id: String,
    clip_id: String,
    include_waveform: bool,
    repaint: Option<egui::Context>,
}

pub(crate) struct AsyncSourceMediaAssetLoader {
    tx: Sender<SourceMediaAssetRequest>,
    rx: Receiver<SourceMediaAssetResult>,
    requested_key: Option<String>,
}

impl Default for AsyncSourceMediaAssetLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncSourceMediaAssetLoader {
    pub fn new() -> Self {
        let (tx_req, rx_req) = mpsc::channel::<SourceMediaAssetRequest>();
        let (tx_res, rx_res) = mpsc::channel::<SourceMediaAssetResult>();

        let _ = thread::Builder::new()
            .name("qnc-source-media-assets".into())
            .spawn(move || {
                while let Ok(mut req) = rx_req.recv() {
                    while let Ok(next) = rx_req.try_recv() {
                        req = next;
                    }

                    let media = load_source_media(
                        &req.host,
                        &req.project_id,
                        &req.clip_id,
                        req.include_waveform,
                    );
                    let _ = tx_res.send(SourceMediaAssetResult {
                        project_id: req.project_id,
                        clip_id: req.clip_id,
                        media,
                    });
                    if let Some(ctx) = req.repaint {
                        ctx.request_repaint();
                    }
                }
            });

        Self {
            tx: tx_req,
            rx: rx_res,
            requested_key: None,
        }
    }

    pub fn request(
        &mut self,
        host: &HostClient,
        project_id: String,
        clip_id: String,
        include_waveform: bool,
        repaint: Option<egui::Context>,
    ) -> bool {
        if project_id.trim().is_empty() || clip_id.trim().is_empty() {
            return false;
        }
        let key = source_media_key(&project_id, &clip_id);
        if self.requested_key.as_deref() == Some(key.as_str()) {
            return false;
        }
        if self
            .tx
            .send(SourceMediaAssetRequest {
                host: host.clone(),
                project_id,
                clip_id,
                include_waveform,
                repaint,
            })
            .is_err()
        {
            return false;
        }
        self.requested_key = Some(key);
        true
    }

    pub fn poll(&mut self) -> Vec<SourceMediaAssetResult> {
        let mut out = Vec::new();
        while let Ok(result) = self.rx.try_recv() {
            if self.requested_key.as_deref()
                == Some(source_media_key(&result.project_id, &result.clip_id).as_str())
            {
                self.requested_key = None;
            }
            out.push(result);
        }
        out
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WaveformAsset {
    pub a1_peaks: Vec<f32>,
    pub a2_peaks: Vec<f32>,
}

pub(crate) struct WaveformAssetResult {
    pub project_id: String,
    pub clip_id: String,
    pub waveform: Result<WaveformAsset, String>,
}

struct WaveformAssetRequest {
    host: HostClient,
    project_id: String,
    clip_id: String,
    repaint: Option<egui::Context>,
}

pub(crate) struct AsyncWaveformAssetLoader {
    tx: Sender<WaveformAssetRequest>,
    rx: Receiver<WaveformAssetResult>,
    in_flight: HashSet<String>,
}

impl Default for AsyncWaveformAssetLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncWaveformAssetLoader {
    pub fn new() -> Self {
        let (tx_req, rx_req) = mpsc::channel::<WaveformAssetRequest>();
        let (tx_res, rx_res) = mpsc::channel::<WaveformAssetResult>();

        let _ = thread::Builder::new()
            .name("qnc-waveform-assets".into())
            .spawn(move || {
                while let Ok(req) = rx_req.recv() {
                    let waveform = load_waveform_asset(&req.host, &req.project_id, &req.clip_id);
                    let _ = tx_res.send(WaveformAssetResult {
                        project_id: req.project_id,
                        clip_id: req.clip_id,
                        waveform,
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
        host: &HostClient,
        project_id: String,
        clip_id: String,
        repaint: Option<egui::Context>,
    ) -> bool {
        if project_id.trim().is_empty() || clip_id.trim().is_empty() {
            return false;
        }
        let key = source_media_key(&project_id, &clip_id);
        if self.in_flight.contains(&key) {
            return false;
        }
        if self
            .tx
            .send(WaveformAssetRequest {
                host: host.clone(),
                project_id,
                clip_id,
                repaint,
            })
            .is_err()
        {
            return false;
        }
        self.in_flight.insert(key);
        true
    }

    pub fn poll(&mut self) -> Vec<WaveformAssetResult> {
        let mut out = Vec::new();
        while let Ok(result) = self.rx.try_recv() {
            self.in_flight
                .remove(&source_media_key(&result.project_id, &result.clip_id));
            out.push(result);
        }
        out
    }
}

pub(crate) fn ingest_thumbnail_url(host: &HostClient, project_id: &str, clip_id: &str) -> String {
    host.absolute(&format!(
        "/api/ingest/thumbnail?project_id={}&clip_id={}",
        api::encode_query_value(project_id),
        api::encode_query_value(clip_id)
    ))
}

pub(crate) fn story_thumbnail_url(
    host: &HostClient,
    project_id: &str,
    clip_id: &str,
    seek_sec: f64,
) -> String {
    host.absolute(&format!(
        "/api/story/thumbnail?project_id={}&clip_id={}&seek={seek_sec:.3}",
        api::encode_query_value(project_id),
        api::encode_query_value(clip_id)
    ))
}

fn load_source_media(
    host: &HostClient,
    project_id: &str,
    clip_id: &str,
    include_waveform: bool,
) -> Result<SourceMediaAsset, String> {
    let _ = host.request_json(
        HostRequestMethod::Post,
        "/api/story/timeline/build",
        Some(json!({
            "project_id": project_id,
            "clip_id": clip_id,
            "frames": SOURCE_FILMSTRIP_FRAME_COUNT
        })),
        HostRequestTimeout::Default,
    );
    let filmstrip = filmstrip_frames(host, project_id, clip_id).unwrap_or_default();
    let (a1_peaks, a2_peaks) = if include_waveform {
        (
            waveform_peaks(host, project_id, clip_id, 1).unwrap_or_default(),
            waveform_peaks(host, project_id, clip_id, 2).unwrap_or_default(),
        )
    } else {
        (Vec::new(), Vec::new())
    };
    Ok(SourceMediaAsset {
        clip_id: clip_id.to_string(),
        a1_peaks,
        a2_peaks,
        waveform_loaded: include_waveform,
        film_frames: filmstrip.frames,
        filmstrip_ready: filmstrip.ready,
        filmstrip_status: filmstrip.status,
        filmstrip_error: filmstrip.error,
    })
}

fn load_waveform_asset(
    host: &HostClient,
    project_id: &str,
    clip_id: &str,
) -> Result<WaveformAsset, String> {
    let a1_peaks = waveform_peaks(host, project_id, clip_id, 1).unwrap_or_default();
    let a2_peaks = waveform_peaks(host, project_id, clip_id, 2).unwrap_or_default();
    if a1_peaks.is_empty() && a2_peaks.is_empty() {
        return Err(format!("waveform nije spreman · {clip_id}"));
    }
    Ok(WaveformAsset { a1_peaks, a2_peaks })
}

fn waveform_peaks(
    host: &HostClient,
    project_id: &str,
    clip_id: &str,
    channel: u8,
) -> Result<Vec<f32>, String> {
    let value = host.request_json(
        HostRequestMethod::Get,
        &format!(
            "/api/ingest/waveform/peaks?project_id={}&clip_id={}&channel={channel}",
            api::encode_query_value(project_id),
            api::encode_query_value(clip_id)
        ),
        None,
        HostRequestTimeout::Default,
    )?;
    Ok(value
        .get("peaks")
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_f64().map(|f| f as f32))
                .collect()
        })
        .unwrap_or_default())
}

#[derive(Default)]
struct SourceFilmstripAsset {
    ready: bool,
    status: String,
    error: String,
    frames: Vec<SourceFilmFrameAsset>,
}

fn filmstrip_frames(
    host: &HostClient,
    project_id: &str,
    clip_id: &str,
) -> Result<SourceFilmstripAsset, String> {
    let value = host.request_json(
        HostRequestMethod::Get,
        &format!(
            "/api/story/filmstrip?project_id={}&clip_id={}",
            api::encode_query_value(project_id),
            api::encode_query_value(clip_id)
        ),
        None,
        HostRequestTimeout::Default,
    )?;
    let status = value
        .get("filmstrip")
        .and_then(|f| f.get("status"))
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let error = value
        .get("filmstrip")
        .and_then(|f| f.get("error"))
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let ready = status == "ready";
    let frames = value
        .get("frames")
        .and_then(|f| f.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(SourceFilmstripAsset {
        ready,
        status,
        error,
        frames: frames
            .into_iter()
            .filter_map(|frame| {
                let index = frame.get("index").and_then(|x| x.as_i64()).unwrap_or(0);
                let seek_sec = frame
                    .get("seek_sec")
                    .and_then(|x| x.as_f64())
                    .unwrap_or(0.0);
                let rel = frame.get("url").and_then(|x| x.as_str())?;
                Some(SourceFilmFrameAsset {
                    index,
                    seek_sec,
                    url: host.absolute(rel),
                })
            })
            .collect(),
    })
}

fn download_color_image_url(url: &str) -> Result<ColorImage, String> {
    let bytes = download_bytes_url(url)?;
    color_image_from_bytes(&bytes)
}

fn color_image_from_bytes(bytes: &[u8]) -> Result<ColorImage, String> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, &img.into_raw()))
}

fn download_bytes_url(url: &str) -> Result<Vec<u8>, String> {
    let mut reader = ureq::AgentBuilder::new()
        .timeout_connect(DOWNLOAD_CONNECT_TIMEOUT)
        .timeout_read(DOWNLOAD_READ_TIMEOUT)
        .build()
        .get(url)
        .call()
        .map_err(|e| e.to_string())?
        .into_reader();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

fn source_media_key(project_id: &str, clip_id: &str) -> String {
    format!("{project_id}\u{1f}{clip_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_asset_key_is_scope_item_variant_scoped() {
        let a = ImageAssetKey::new("thumb", "clip-1", "poster");
        let b = ImageAssetKey::new("thumb", "clip-1", "film-1");
        assert_ne!(a, b);
    }

    #[test]
    fn thumbnail_urls_are_encoded_relative_assets() {
        let host = HostClient::new("http://127.0.0.1:8001");
        let url = ingest_thumbnail_url(&host, "p 1", "c/2");
        assert_eq!(
            url,
            "http://127.0.0.1:8001/api/ingest/thumbnail?project_id=p%201&clip_id=c%2F2"
        );
    }

    #[test]
    fn image_loader_caps_in_flight_backlog() {
        let mut loader = AsyncImageAssetLoader::new();
        let mut accepted = 0;
        for i in 0..(IMAGE_ASSET_MAX_IN_FLIGHT + 16) {
            if loader.request(
                ImageAssetKey::new("stress", format!("clip_{i}"), "poster"),
                format!("http://127.0.0.1:9/{i}.jpg"),
                None,
            ) {
                accepted += 1;
            }
        }

        assert_eq!(accepted, IMAGE_ASSET_MAX_IN_FLIGHT);
        assert_eq!(loader.in_flight_len(), IMAGE_ASSET_MAX_IN_FLIGHT);
        assert!(loader.is_saturated());
    }

    #[test]
    fn image_loader_cancel_pending_releases_backlog() {
        let mut loader = AsyncImageAssetLoader::new();
        for i in 0..IMAGE_ASSET_MAX_IN_FLIGHT {
            assert!(loader.request(
                ImageAssetKey::new("stress", format!("clip_{i}"), "poster"),
                format!("http://127.0.0.1:9/{i}.jpg"),
                None,
            ));
        }

        loader.cancel_pending();

        assert_eq!(loader.in_flight_len(), 0);
        assert!(!loader.is_saturated());
        assert!(loader.request(
            ImageAssetKey::new("stress", "after_cancel", "poster"),
            "http://127.0.0.1:9/after_cancel.jpg".into(),
            None,
        ));
    }
}
