//! Shared UI **elements** for editorial screens.
//!
//! **Reference:** Story (then Media Assist). Ingest only *composes* these —
//! it is never the source of layout/spacing/paint.
//!
//! Layers:
//!   Composition → `composition::ScreenComposition` (orchestrator picks blocks)
//!   Layout      → `editorial_shell` / `project_workspace`
//!   Blocks      → `media_column`, `content_panel`
//!   Elements    → `preview`, `chrome_row`, `qnc_form`, media_card, dock/timeline
//!
//! Shell / column APIs use a **single** `FnMut` so screens can capture `&mut self`
//! without overlapping borrows (left then right; chrome then body in one callback).

use eframe::egui::{self, Color32, Rect, Sense, TextureHandle, Vec2};

use crate::qnc_theme::current;

/// Spacing contract — taken from Story shell math (single source).
pub mod space {
    use eframe::egui::Margin;

    use crate::qnc_theme;

    /// Matches source-dock horizontal inset (timeline chrome).
    pub const SHELL_MARGIN_X: i8 = 8;
    /// Story left media column share.
    pub const LEFT_RATIO: f32 = 0.365;
    pub const LEFT_MIN_W: f32 = 280.0;
    pub const DIV_W: f32 = 5.0;
    pub const RIGHT_MIN_W: f32 = 200.0;

    pub const CHROME_H: f32 = qnc_theme::CHROME_ROW_H;
    pub const PAD_X: i8 = qnc_theme::CHROME_PAD_X;
    pub const BLOCK_PAD: i8 = 10;
    pub const GAP: f32 = 8.0;

    /// Story: room under preview for chrome + strip.
    pub const PREVIEW_RESERVE_BELOW: f32 = 190.0;
    pub const PREVIEW_MIN_H: f32 = 160.0;
    pub const BODY_MIN_H: f32 = 96.0;

    pub fn block_margin() -> Margin {
        Margin::symmetric(BLOCK_PAD, BLOCK_PAD)
    }
}

use space::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellSide {
    Left,
    Right,
}

pub struct ShellMetrics {
    pub left_w: f32,
    pub right_w: f32,
    pub height: f32,
}

impl ShellMetrics {
    pub fn from_avail(avail: Vec2) -> Self {
        Self::from_avail_ratio(avail, LEFT_RATIO)
    }

    /// Custom left share (web Project workspace = 0.32).
    pub fn from_avail_ratio(avail: Vec2, left_ratio: f32) -> Self {
        let left_w = (avail.x * left_ratio).max(LEFT_MIN_W);
        let right_w = (avail.x - left_w - DIV_W).max(RIGHT_MIN_W);
        Self {
            left_w,
            right_w,
            height: avail.y,
        }
    }

    /// Story preview height math.
    pub fn preview_h(&self) -> f32 {
        let max_h = (self.height - PREVIEW_RESERVE_BELOW).max(PREVIEW_MIN_H);
        let preview_w = (self.left_w - 32.0).max(240.0);
        (preview_w * 9.0 / 16.0).min(max_h).max(PREVIEW_MIN_H)
    }

    pub fn body_h(&self, preview_h: f32) -> f32 {
        // Never invent height above the column — overflow would paint over the dock.
        (self.height - preview_h - CHROME_H).max(0.0)
    }
}

/// Layout (Story shell): left | divider | right — one callback, sequential sides.
pub fn editorial_shell(
    ui: &mut egui::Ui,
    mut paint: impl FnMut(&mut egui::Ui, &ShellMetrics, ShellSide),
) {
    shell_split(ui, LEFT_RATIO, DIV_W, true, &mut paint);
}

/// Same column shell as Story, with a custom left share.
/// Does not paint a separate left surface — both columns share the shell bg
/// (Project bodies); Story keeps `editorial_shell` + surface for the media col.
pub fn column_shell(
    ui: &mut egui::Ui,
    left_ratio: f32,
    mut paint: impl FnMut(&mut egui::Ui, &ShellMetrics, ShellSide),
) {
    shell_split(ui, left_ratio, DIV_W, false, &mut paint);
}

/// Web `workspace-split` / `.qnc-project-workspace`: ~32% list | settings, 1px seam.
pub const PROJECT_LEFT_RATIO: f32 = 0.32;
const PROJECT_LEFT_MIN: f32 = 260.0;
const PROJECT_RIGHT_MIN: f32 = 360.0;

pub fn project_workspace(
    ui: &mut egui::Ui,
    mut paint: impl FnMut(&mut egui::Ui, &ShellMetrics, ShellSide),
) {
    // Exact rect from CentralPanel — never grow into the shell footer.
    let rect = ui.available_rect_before_wrap();
    ui.allocate_exact_size(rect.size(), egui::Sense::hover());
    ui.set_clip_rect(rect);

    let t = current(ui);
    ui.painter().rect_filled(rect, 0.0, t.bg);

    let div_w = 1.0;
    let total = rect.width();
    // Exact partition: left + div + right == total (no overflow clipping pad).
    let mut left_w = (total * PROJECT_LEFT_RATIO).round().max(PROJECT_LEFT_MIN);
    if left_w + div_w + PROJECT_RIGHT_MIN > total {
        left_w = (total - div_w - PROJECT_RIGHT_MIN).max(180.0);
    }
    let right_w = (total - left_w - div_w).max(0.0);
    let m = ShellMetrics {
        left_w,
        right_w,
        height: rect.height(),
    };

    let left_rect = Rect::from_min_size(rect.min, Vec2::new(left_w, m.height));
    let div_rect = Rect::from_min_size(
        egui::pos2(left_rect.right(), rect.top()),
        Vec2::new(div_w, m.height),
    );
    let right_rect = Rect::from_min_max(egui::pos2(div_rect.right(), rect.top()), rect.max);

    ui.painter().rect_filled(div_rect, 0.0, t.border);

    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(left_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_clip_rect(left_rect);
            paint(ui, &m, ShellSide::Left);
        },
    );
    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(right_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_clip_rect(right_rect);
            paint(ui, &m, ShellSide::Right);
        },
    );
}

fn shell_split(
    ui: &mut egui::Ui,
    left_ratio: f32,
    div_w: f32,
    fill_left_col: bool,
    paint: &mut impl FnMut(&mut egui::Ui, &ShellMetrics, ShellSide),
) {
    let t = current(ui);
    let rect = ui.available_rect_before_wrap();
    ui.allocate_exact_size(rect.size(), egui::Sense::hover());
    ui.set_clip_rect(rect);
    ui.painter().rect_filled(rect, 0.0, t.bg);

    let mut m = ShellMetrics::from_avail_ratio(rect.size(), left_ratio);
    m.right_w = (rect.width() - m.left_w - div_w).max(RIGHT_MIN_W);
    // Re-clamp so columns fit exactly.
    if m.left_w + div_w + m.right_w > rect.width() {
        m.right_w = (rect.width() - m.left_w - div_w).max(0.0);
    }
    m.height = rect.height();

    let left_rect = Rect::from_min_size(rect.min, Vec2::new(m.left_w, m.height));
    let div_rect = Rect::from_min_size(
        egui::pos2(left_rect.right(), rect.top()),
        Vec2::new(div_w, m.height),
    );
    let right_rect = Rect::from_min_max(egui::pos2(div_rect.right(), rect.top()), rect.max);

    ui.painter().rect_filled(div_rect, 0.0, t.border);

    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(left_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_clip_rect(left_rect);
            if fill_left_col {
                ui.painter().rect_filled(left_rect, 0.0, t.surface);
            }
            paint(ui, &m, ShellSide::Left);
        },
    );
    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(right_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_clip_rect(right_rect);
            paint(ui, &m, ShellSide::Right);
        },
    );
}

/// Block (left col): monitor slot, then chrome + body.
///
/// Prefer [`media_column_monitor`] — paint via broadcast monitor, not a raw texture.
///
/// `after_preview` receives the **remaining height after preview** (exact column
/// remainder). Caller must tile chrome + body inside that budget with
/// `item_spacing.y = 0` and size the body from `ui.available_height()` after chrome
/// — never allocate chrome + precomputed body_h (that overflows into the dock).
pub fn media_column(
    ui: &mut egui::Ui,
    m: &ShellMetrics,
    texture: Option<&TextureHandle>,
    empty_label: &str,
    preview_sense: Sense,
    after_preview: impl FnMut(&mut egui::Ui, f32),
) {
    media_column_monitor(ui, m, |ui, preview_h| {
        preview(
            ui,
            PreviewInput {
                height: preview_h,
                texture,
                empty_label,
                sense: preview_sense,
            },
        );
    }, after_preview);
}

/// Left media column with an injected monitor paint (broadcast player monitor).
pub fn media_column_monitor(
    ui: &mut egui::Ui,
    m: &ShellMetrics,
    mut paint_monitor: impl FnMut(&mut egui::Ui, f32),
    mut after_preview: impl FnMut(&mut egui::Ui, f32),
) {
    let preview_h = m.preview_h().min(ui.available_height().max(0.0));
    paint_monitor(ui, preview_h);
    let rest = ui.available_height().max(0.0);
    after_preview(ui, rest);
}

pub struct PreviewInput<'a> {
    pub height: f32,
    pub texture: Option<&'a TextureHandle>,
    pub empty_label: &'a str,
    pub sense: Sense,
}

/// Element: Story preview monitor (contain + centered).
pub fn preview(ui: &mut egui::Ui, input: PreviewInput<'_>) -> egui::Response {
    let t = current(ui);
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, input.height), input.sense);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, t.preview_black);
    if let Some(tex) = input.texture {
        let size = tex.size_vec2();
        let scale = (rect.width() / size.x).min(rect.height() / size.y);
        let draw = size * scale;
        let offset = (rect.size() - draw) * 0.5;
        let img = Rect::from_min_size(rect.min + offset, draw);
        painter.image(
            tex.id(),
            img,
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            input.empty_label,
            egui::FontId::proportional(14.0),
            t.muted,
        );
    }
    resp
}

/// Content block under chrome (fixed height, padded). Clipped — cannot cover dock.
///
/// Always paints a full-rect face (`t.bg`) so the panel is one solid block even
/// when inner content is shorter than `height`.
pub fn content_panel(ui: &mut egui::Ui, height: f32, add_contents: impl FnOnce(&mut egui::Ui)) {
    let t = current(ui);
    let width = ui.available_width();
    let height = height.max(0.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, t.bg);
    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_clip_rect(rect);
            egui::Frame::NONE
                .inner_margin(space::block_margin())
                .show(ui, |ui| {
                    add_contents(ui);
                });
        },
    );
}
