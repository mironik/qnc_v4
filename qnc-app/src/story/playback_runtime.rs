//! Story media download helpers (thumbs / filmstrip).
//!
//! Preview decode belongs to the broadcast player (`PlayerRemote`), not host JPEG.

use std::io::Read;
use std::time::Duration;

use eframe::egui::ColorImage;

use crate::api::HostClient;

const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(120);

pub(super) fn download_color_image(host: &HostClient, url: &str) -> Result<ColorImage, String> {
    let bytes = host.download_bytes(url)?;
    color_image_from_bytes(&bytes)
}

pub(super) fn download_color_image_url(url: &str) -> Result<ColorImage, String> {
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
