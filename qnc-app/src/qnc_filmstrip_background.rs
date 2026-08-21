//! Passive filmstrip background UI data.
//!
//! Filmstrip frames are thumbnail/background material for UI inspection only.
//! They are not a player source, not a timeline clock, and not part of playback
//! math.

use eframe::egui::{self, Color32, TextureHandle, Vec2};

const THUMB_W: f32 = 112.0;
const SEAM: Color32 = Color32::from_rgb(0x11, 0x18, 0x27);
const FRAME: Color32 = Color32::from_rgb(0x1f, 0x29, 0x37);

#[derive(Clone, Default)]
pub struct FilmFrame {
    pub index: i64,
    #[allow(dead_code)]
    pub seek_sec: f64,
    pub url: String,
    pub texture: Option<TextureHandle>,
    pub load_attempts: u8,
}

pub fn merge_frames(existing: &mut Vec<FilmFrame>, frames: impl IntoIterator<Item = FilmFrame>) {
    let old = std::mem::take(existing);
    *existing = frames
        .into_iter()
        .map(|mut frame| {
            frame.texture = old
                .iter()
                .find(|existing| existing.index == frame.index && existing.url == frame.url)
                .and_then(|existing| existing.texture.clone());
            frame.load_attempts = old
                .iter()
                .find(|existing| existing.index == frame.index && existing.url == frame.url)
                .map(|existing| existing.load_attempts)
                .unwrap_or(0);
            frame
        })
        .collect();
}

pub fn paint(ui: &mut egui::Ui, area: egui::Rect, frames: &[FilmFrame]) {
    paint_slots(ui.painter(), area, frames);
    put_images(ui, area, frames);
}

fn paint_slots(painter: &egui::Painter, area: egui::Rect, frames: &[FilmFrame]) {
    let n = frames.len().max(1);
    let slot_w = (area.width() / n as f32).clamp(8.0, THUMB_W);
    for (i, frame) in frames.iter().enumerate() {
        let x0 = area.left() + i as f32 * slot_w;
        if x0 >= area.right() {
            break;
        }
        let slot = egui::Rect::from_min_max(
            egui::pos2(x0, area.top() + 1.0),
            egui::pos2((x0 + slot_w - 1.0).min(area.right()), area.bottom() - 1.0),
        );
        painter.rect_filled(slot, 0.0, FRAME);
        if frame.texture.is_none() {
            painter.rect_filled(slot.shrink(2.0), 0.0, SEAM);
        }
        painter.vline(slot.right(), slot.y_range(), egui::Stroke::new(1.0, SEAM));
    }
}

fn put_images(ui: &mut egui::Ui, area: egui::Rect, frames: &[FilmFrame]) {
    let n = frames.len().max(1);
    let slot_w = (area.width() / n as f32).clamp(8.0, THUMB_W);
    for (i, frame) in frames.iter().enumerate() {
        let Some(tex) = &frame.texture else {
            continue;
        };
        let x0 = area.left() + i as f32 * slot_w;
        if x0 >= area.right() {
            break;
        }
        let slot = egui::Rect::from_min_max(
            egui::pos2(x0, area.top() + 1.0),
            egui::pos2((x0 + slot_w - 1.0).min(area.right()), area.bottom() - 1.0),
        );
        let size = tex.size_vec2();
        let scale = (slot.width() / size.x).min(slot.height() / size.y);
        let img_size: Vec2 = size * scale;
        ui.put(
            egui::Rect::from_center_size(slot.center(), img_size),
            egui::Image::new((tex.id(), img_size)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_frames_keeps_manifest_order_and_replaces_urls() {
        let mut existing = vec![FilmFrame {
            index: 0,
            seek_sec: 0.0,
            url: "/old.jpg".into(),
            texture: None,
            load_attempts: 0,
        }];

        merge_frames(
            &mut existing,
            vec![
                FilmFrame {
                    index: 1,
                    seek_sec: 1.0,
                    url: "/b.jpg".into(),
                    texture: None,
                    load_attempts: 0,
                },
                FilmFrame {
                    index: 0,
                    seek_sec: 0.0,
                    url: "/a.jpg".into(),
                    texture: None,
                    load_attempts: 0,
                },
            ],
        );

        assert_eq!(
            existing.iter().map(|frame| frame.index).collect::<Vec<_>>(),
            vec![1, 0]
        );
        assert_eq!(existing[1].url, "/a.jpg");
    }
}
