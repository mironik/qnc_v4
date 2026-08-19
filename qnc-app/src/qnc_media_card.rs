//! Universal media card (thumb + footer) — one element for Story / MA / Ingest.
//!
//! Orchestrators choose features (`MediaCardFeatures`); paint never forks per screen.
//! Visual contract mirrors web clip card (footer name · status dots · duration).

use eframe::egui::{self, Color32, Rect, TextureHandle, Vec2};

use crate::qnc_theme::{self, BORDER, MUTED, RAISED, SELECT_RED, SURFACE, TEXT};

pub const GRID_GAP: f32 = 10.0;
pub const CARD_TEXT_H: f32 = 34.0;
pub const MIN_CARD_W: f32 = 160.0;
pub const MIN_CARD_H: f32 = MIN_CARD_W * 9.0 / 16.0 + CARD_TEXT_H;

/// How status dots next to the filename are shown (same paint path for all screens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusDotsMode {
    /// No dots (unused — prefer explicit presets).
    #[allow(dead_code)]
    Off,
    /// Story / MA: proxy + original (ready/pending/error colours) once import started.
    Pipeline,
    /// Ingest: one green “imported” dot next to the name — only when DB says imported/done.
    /// No red/yellow pending dots.
    ImportedOnly,
}

/// Per-screen feature switches — set by the orchestrator, not by the element.
#[derive(Debug, Clone, Copy)]
pub struct MediaCardFeatures {
    /// Bottom-left check mark (Ingest / Media Assist). Off in Story.
    pub selection_check: bool,
    /// Status dots beside the filename.
    pub status_dots: StatusDotsMode,
}

impl MediaCardFeatures {
    pub const STORY: Self = Self {
        selection_check: false,
        status_dots: StatusDotsMode::Pipeline,
    };
    pub const MEDIA_ASSIST: Self = Self {
        selection_check: true,
        status_dots: StatusDotsMode::Pipeline,
    };
    pub const INGEST: Self = Self {
        selection_check: true,
        status_dots: StatusDotsMode::ImportedOnly,
    };
}

#[derive(Debug, Clone, Copy)]
pub struct GridMetrics {
    pub cols: usize,
    pub card_w: f32,
    pub card_h: f32,
    pub gap: f32,
}

pub fn grid_metrics(available_w: f32, count: usize) -> GridMetrics {
    let count = count.max(1);
    let usable_w = (available_w - 8.0).max(MIN_CARD_W);
    let cols = (((usable_w + GRID_GAP) / (MIN_CARD_W + GRID_GAP)).floor() as usize)
        .max(1)
        .min(count);
    let card_w = (usable_w - GRID_GAP * cols.saturating_sub(1) as f32) / cols as f32;
    let card_h = card_w * 9.0 / 16.0 + CARD_TEXT_H;
    GridMetrics {
        cols,
        card_w,
        card_h,
        gap: GRID_GAP,
    }
}

/// Neutral card model — no Story/Ingest types.
pub struct MediaCardInput<'a> {
    pub title: &'a str,
    pub duration_sec: f64,
    /// Prefer when non-empty (e.g. Story `duration_label`).
    pub duration_label: &'a str,
    pub import_status: &'a str,
    pub status_proxy: &'a str,
    pub status_original: &'a str,
    /// Preview / focus ring (red border). Independent of checkbox.
    pub focused: bool,
    /// Multi-select / checked state (only painted if `features.selection_check`).
    pub checked: bool,
    pub features: MediaCardFeatures,
    pub thumb: Option<&'a TextureHandle>,
    pub tc: &'a dyn Fn(f64) -> String,
}

pub fn paint_media_card(ui: &egui::Ui, rect: Rect, input: &MediaCardInput<'_>) {
    let painter = ui.painter_at(rect);
    let stroke = egui::Stroke::new(
        if input.focused { 2.0 } else { 1.0 },
        if input.focused { SELECT_RED } else { BORDER },
    );
    painter.rect_filled(rect, 0.0, RAISED);
    painter.rect_stroke(rect, 0.0, stroke, egui::StrokeKind::Inside);

    let thumb_h = (rect.height() - CARD_TEXT_H).max(72.0);
    let thumb_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), thumb_h));
    painter.rect_filled(thumb_rect, 0.0, SURFACE);
    if let Some(tex) = input.thumb {
        let size = tex.size_vec2();
        if size.x > 0.0 && size.y > 0.0 {
            let scale = (thumb_rect.width() / size.x).max(thumb_rect.height() / size.y);
            let image_rect = Rect::from_center_size(thumb_rect.center(), size * scale);
            ui.painter_at(thumb_rect).image(
                tex.id(),
                image_rect,
                Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    } else {
        painter.text(
            thumb_rect.center(),
            egui::Align2::CENTER_CENTER,
            "…",
            egui::TextStyle::Body.resolve(ui.style()),
            MUTED,
        );
    }

    if input.features.selection_check {
        paint_selection_check(ui, thumb_rect, input.checked);
    }

    let dur = if !input.duration_label.trim().is_empty() {
        input.duration_label.to_string()
    } else {
        (input.tc)(input.duration_sec)
    };
    let meta = Rect::from_min_max(egui::pos2(rect.left(), thumb_rect.bottom()), rect.max);
    painter.rect_filled(meta, 0.0, RAISED);
    let text_y = meta.center().y;
    let small = egui::TextStyle::Small.resolve(ui.style());

    let dots = status_dots_layout(input);
    let dots_w = match dots {
        StatusDotsLayout::None => 0.0,
        StatusDotsLayout::One => 12.0,
        StatusDotsLayout::Two => 22.0,
    };
    let name_max_w = (rect.width() - 16.0 - 52.0 - dots_w).max(40.0);
    let name = qnc_theme::truncate(
        input.title,
        (name_max_w / 7.0).floor().clamp(8.0, 42.0) as usize,
    );
    let name_pos = egui::pos2(rect.left() + 8.0, text_y);
    let name_w = ui.fonts(|f| f.layout_no_wrap(name.clone(), small.clone(), TEXT).size().x);
    painter.text(
        name_pos,
        egui::Align2::LEFT_CENTER,
        name,
        small.clone(),
        TEXT,
    );

    let dx = rect.left() + 8.0 + name_w + 8.0;
    match dots {
        StatusDotsLayout::None => {}
        StatusDotsLayout::One => {
            painter.circle_filled(egui::pos2(dx, text_y), 3.5, DOT_READY_GREEN);
        }
        StatusDotsLayout::Two => {
            painter.circle_filled(
                egui::pos2(dx, text_y),
                3.5,
                proxy_dot_color(input.status_proxy),
            );
            painter.circle_filled(
                egui::pos2(dx + 10.0, text_y),
                3.5,
                original_dot_color(input.status_original),
            );
        }
    }

    painter.text(
        egui::pos2(rect.right() - 8.0, text_y),
        egui::Align2::RIGHT_CENTER,
        dur,
        small,
        MUTED,
    );
}

pub fn selection_check_hit_rect(card_rect: Rect) -> Rect {
    selection_check_rect(thumb_rect(card_rect)).expand(4.0)
}

#[derive(Clone, Copy)]
enum StatusDotsLayout {
    None,
    One,
    Two,
}

fn status_dots_layout(input: &MediaCardInput<'_>) -> StatusDotsLayout {
    match input.features.status_dots {
        StatusDotsMode::Off => StatusDotsLayout::None,
        StatusDotsMode::Pipeline if import_started(input.import_status) => StatusDotsLayout::Two,
        StatusDotsMode::Pipeline => StatusDotsLayout::None,
        StatusDotsMode::ImportedOnly if is_imported_status(input.import_status) => {
            StatusDotsLayout::One
        }
        StatusDotsMode::ImportedOnly => StatusDotsLayout::None,
    }
}

const DOT_READY_GREEN: Color32 = Color32::from_rgb(0x30, 0xd1, 0x58);

/// Ingest-style check: bottom-left on thumb (web `.qnc-ip-clip-check`).
fn paint_selection_check(ui: &egui::Ui, thumb_rect: Rect, checked: bool) {
    let check_rect = selection_check_rect(thumb_rect);
    let painter = ui.painter_at(thumb_rect);
    if checked {
        let fill = Color32::from_rgb(0xff, 0x95, 0x00);
        painter.rect_filled(check_rect, 3.0, fill);
        painter.rect_stroke(
            check_rect,
            3.0,
            egui::Stroke::new(1.5, fill),
            egui::StrokeKind::Inside,
        );
        let c = check_rect.center();
        let dark = Color32::from_rgb(0x1a, 0x1a, 0x1a);
        painter.line_segment(
            [egui::pos2(c.x - 3.5, c.y), egui::pos2(c.x - 1.0, c.y + 3.0)],
            egui::Stroke::new(2.0, dark),
        );
        painter.line_segment(
            [
                egui::pos2(c.x - 1.0, c.y + 3.0),
                egui::pos2(c.x + 4.0, c.y - 3.0),
            ],
            egui::Stroke::new(2.0, dark),
        );
    } else {
        painter.rect_filled(
            check_rect,
            3.0,
            Color32::from_rgba_unmultiplied(0, 0, 0, 90),
        );
        painter.rect_stroke(
            check_rect,
            3.0,
            egui::Stroke::new(1.5, Color32::from_rgba_unmultiplied(255, 255, 255, 140)),
            egui::StrokeKind::Inside,
        );
    }
}

fn thumb_rect(card_rect: Rect) -> Rect {
    let thumb_h = (card_rect.height() - CARD_TEXT_H).max(72.0);
    Rect::from_min_size(card_rect.min, Vec2::new(card_rect.width(), thumb_h))
}

fn selection_check_rect(thumb_rect: Rect) -> Rect {
    let size = 16.0;
    let pad = 6.0;
    Rect::from_min_size(
        egui::pos2(thumb_rect.left() + pad, thumb_rect.bottom() - pad - size),
        Vec2::splat(size),
    )
}

fn proxy_dot_color(status: &str) -> Color32 {
    match status.trim().to_ascii_lowercase().as_str() {
        "ready" => DOT_READY_GREEN,
        "pending" => Color32::from_rgb(0xff, 0xd6, 0x0a),
        _ => Color32::from_rgb(0xff, 0x45, 0x3a),
    }
}

fn original_dot_color(status: &str) -> Color32 {
    match status.trim().to_ascii_lowercase().as_str() {
        "ready" => Color32::from_rgb(0x0a, 0x84, 0xff),
        "pending" => Color32::from_rgb(0xff, 0xd6, 0x0a),
        _ => Color32::from_rgb(0xff, 0x45, 0x3a),
    }
}

fn is_imported_status(import_status: &str) -> bool {
    matches!(
        import_status.trim().to_ascii_lowercase().as_str(),
        "imported" | "done"
    )
}

fn import_started(import_status: &str) -> bool {
    matches!(
        import_status.trim().to_ascii_lowercase().as_str(),
        "queued"
            | "processing"
            | "original_ready"
            | "generating_proxy"
            | "imported"
            | "done"
            | "error"
    )
}
