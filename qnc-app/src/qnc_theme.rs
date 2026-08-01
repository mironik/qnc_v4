//! Shared visual tokens for all native QNC forms (Story / Ingest / Media Assist).
//! Runtime theme selection — helpers read `current(ui)`.

use eframe::egui::{self, Color32, FontFamily, FontId, RichText, Sense, TextStyle, Vec2};

/// Body / button / tab text size (Media Assist pool head + dock actions).
pub const FONT_UI: f32 = 14.0;
pub const FONT_TC: f32 = 13.0;
/// Shared chrome row (pool head / dock header above timeline).
pub const CHROME_ROW_H: f32 = 28.0;
pub const CHROME_PAD_X: i8 = 8;
pub const CHROME_PAD_Y: i8 = 2;
/// Control height inside chrome (fits CHROME_ROW_H − 2×PAD_Y).
pub const CHROME_CTRL_H: f32 = 24.0;

/// Dark-theme aliases (compile compatibility). Prefer `current(ui)` for themed paint.
pub const BG: Color32 = Color32::from_rgb(11, 15, 25);
pub const SURFACE: Color32 = Color32::from_rgb(17, 24, 39);
pub const RAISED: Color32 = Color32::from_rgb(31, 41, 55);
pub const BORDER: Color32 = Color32::from_rgb(55, 65, 81);
pub const TEXT: Color32 = Color32::from_rgb(229, 231, 235);
pub const MUTED: Color32 = Color32::from_rgb(156, 163, 175);
pub const ACCENT: Color32 = Color32::from_rgb(16, 185, 129);
pub const SELECT_RED: Color32 = Color32::from_rgb(239, 68, 68);
pub const TEAL_WAVE: Color32 = Color32::from_rgb(45, 212, 191);
pub const PREVIEW_BLACK: Color32 = Color32::BLACK;
pub const TC_GOLD: Color32 = Color32::from_rgb(0xf0, 0xb4, 0x00);
pub const FOCUS: Color32 = Color32::from_rgb(255, 180, 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ThemeId {
    #[default]
    Dark,
    Soft,
    HighContrast,
}

impl ThemeId {
    pub const ALL: [ThemeId; 3] = [ThemeId::Dark, ThemeId::Soft, ThemeId::HighContrast];

    pub fn as_str(self) -> &'static str {
        match self {
            ThemeId::Dark => "dark",
            ThemeId::Soft => "soft",
            ThemeId::HighContrast => "high_contrast",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ThemeId::Dark => "Dark",
            ThemeId::Soft => "Soft",
            ThemeId::HighContrast => "High contrast",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(ThemeId::Dark),
            "soft" => Some(ThemeId::Soft),
            "high_contrast" | "high-contrast" | "contrast" => Some(ThemeId::HighContrast),
            _ => None,
        }
    }

    pub fn tokens(self) -> ThemeTokens {
        match self {
            ThemeId::Dark => ThemeTokens::dark(),
            ThemeId::Soft => ThemeTokens::soft(),
            ThemeId::HighContrast => ThemeTokens::high_contrast(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ThemeTokens {
    pub bg: Color32,
    pub surface: Color32,
    pub raised: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub select_red: Color32,
    pub teal_wave: Color32,
    pub preview_black: Color32,
    pub tc_gold: Color32,
    pub focus: Color32,
}

/// Timeline paint colors derived from the active theme (not a fixed dark palette).
#[derive(Debug, Clone, Copy)]
pub struct TimelineColors {
    pub bg: Color32,
    pub video_bg: Color32,
    pub audio_primary_bg: Color32,
    pub audio_secondary_bg: Color32,
    pub label_bg: Color32,
    pub line: Color32,
    pub filmstrip_seam: Color32,
    pub filmstrip_frame: Color32,
    pub wave_a1: Color32,
    pub wave_a2: Color32,
    pub focus: Color32,
    pub playhead: Color32,
    pub muted: Color32,
    pub io_handle: Color32,
}

impl ThemeTokens {
    pub fn timeline(self) -> TimelineColors {
        TimelineColors {
            bg: self.bg,
            video_bg: self.surface,
            audio_primary_bg: self.surface,
            audio_secondary_bg: self.bg,
            label_bg: self.raised,
            line: self.border,
            filmstrip_seam: self.surface,
            filmstrip_frame: self.raised,
            wave_a1: self.accent,
            wave_a2: self.muted,
            focus: self.focus,
            playhead: self.teal_wave,
            muted: self.muted,
            io_handle: Color32::WHITE,
        }
    }

    pub fn dark() -> Self {
        Self {
            bg: BG,
            surface: SURFACE,
            raised: RAISED,
            border: BORDER,
            text: TEXT,
            muted: MUTED,
            accent: ACCENT,
            select_red: SELECT_RED,
            teal_wave: TEAL_WAVE,
            preview_black: PREVIEW_BLACK,
            tc_gold: TC_GOLD,
            focus: FOCUS,
        }
    }

    pub fn soft() -> Self {
        Self {
            bg: Color32::from_rgb(22, 27, 38),
            surface: Color32::from_rgb(32, 40, 56),
            raised: Color32::from_rgb(45, 55, 74),
            border: Color32::from_rgb(75, 88, 110),
            text: Color32::from_rgb(236, 239, 244),
            muted: Color32::from_rgb(168, 178, 194),
            accent: Color32::from_rgb(52, 199, 148),
            select_red: Color32::from_rgb(248, 113, 113),
            teal_wave: Color32::from_rgb(94, 234, 212),
            preview_black: Color32::from_rgb(8, 10, 14),
            tc_gold: Color32::from_rgb(251, 191, 36),
            focus: Color32::from_rgb(255, 196, 90),
        }
    }

    pub fn high_contrast() -> Self {
        Self {
            bg: Color32::from_rgb(0, 0, 0),
            surface: Color32::from_rgb(18, 18, 18),
            raised: Color32::from_rgb(36, 36, 36),
            border: Color32::from_rgb(180, 180, 180),
            text: Color32::from_rgb(255, 255, 255),
            muted: Color32::from_rgb(200, 200, 200),
            accent: Color32::from_rgb(0, 255, 170),
            select_red: Color32::from_rgb(255, 80, 80),
            teal_wave: Color32::from_rgb(0, 230, 200),
            preview_black: Color32::BLACK,
            tc_gold: Color32::from_rgb(255, 220, 0),
            focus: Color32::from_rgb(255, 200, 0),
        }
    }
}

fn theme_id_key() -> egui::Id {
    egui::Id::new("qnc_theme_id")
}

pub fn set_active(ctx: &egui::Context, id: ThemeId) {
    ctx.data_mut(|d| d.insert_temp(theme_id_key(), id));
}

pub fn active_id(ctx: &egui::Context) -> ThemeId {
    ctx.data(|d| d.get_temp::<ThemeId>(theme_id_key()).unwrap_or_default())
}

pub fn current(ui: &egui::Ui) -> ThemeTokens {
    active_id(ui.ctx()).tokens()
}

pub fn current_ctx(ctx: &egui::Context) -> ThemeTokens {
    active_id(ctx).tokens()
}

/// Apply panel / widget fills from tokens (keeps text styles from `apply_app_fonts`).
pub fn apply_egui_visuals(ctx: &egui::Context, tokens: &ThemeTokens) {
    let mut style = (*ctx.style()).clone();
    // Comfortable hit targets — avoids text glued to button edges.
    style.spacing.button_padding = Vec2::new(10.0, 6.0);
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.visuals.panel_fill = tokens.bg;
    style.visuals.window_fill = tokens.surface;
    style.visuals.extreme_bg_color = tokens.bg;
    style.visuals.faint_bg_color = tokens.surface;
    style.visuals.code_bg_color = tokens.raised;
    style.visuals.override_text_color = Some(tokens.text);
    style.visuals.widgets.noninteractive.bg_fill = tokens.surface;
    style.visuals.widgets.noninteractive.weak_bg_fill = tokens.raised;
    style.visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, tokens.muted);
    style.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, tokens.border);
    style.visuals.widgets.inactive.bg_fill = tokens.raised;
    style.visuals.widgets.inactive.weak_bg_fill = tokens.surface;
    style.visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, tokens.text);
    style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, tokens.border);
    style.visuals.widgets.hovered.bg_fill = tokens.raised;
    style.visuals.widgets.hovered.weak_bg_fill = tokens.raised;
    style.visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, tokens.text);
    style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, tokens.accent);
    style.visuals.widgets.active.bg_fill = tokens.accent;
    style.visuals.widgets.active.weak_bg_fill = tokens.accent;
    style.visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, Color32::WHITE);
    style.visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, tokens.accent);
    style.visuals.selection.bg_fill = tokens.accent.linear_multiply(0.35);
    style.visuals.selection.stroke = egui::Stroke::new(1.0, tokens.accent);
    style.visuals.hyperlink_color = tokens.accent;
    ctx.set_style(style);
}

/// Global readable type scale (Story / Ingest / Media Assist).
pub fn apply_app_fonts(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(13.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Body,
        FontId::new(FONT_UI, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(FONT_UI, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(20.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(FONT_UI, FontFamily::Monospace),
    );
    // Comfortable hit targets — avoids text glued to button edges.
    style.spacing.button_padding = Vec2::new(10.0, 6.0);
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    ctx.set_style(style);
}

/// Fixed-height chrome strip: 2px vertical pad + optional full-width bottom rule.
/// Height is exact (`CHROME_ROW_H`) so dock layout math stays correct.
pub fn chrome_row(
    ui: &mut egui::Ui,
    draw_bottom_rule: bool,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    chrome_row_fill(ui, draw_bottom_rule, current(ui).surface, add_contents)
}

/// Panel title strip — same geometry as [`chrome_row`], but **no surface/selection
/// fill** (uses panel `bg`). For Project `Projekti` / `Postavke` headers.
pub fn panel_title_row(
    ui: &mut egui::Ui,
    draw_bottom_rule: bool,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    chrome_row_fill(ui, draw_bottom_rule, current(ui).bg, add_contents)
}

fn chrome_row_fill(
    ui: &mut egui::Ui,
    draw_bottom_rule: bool,
    fill: Color32,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let t = current(ui);
    let width = ui.available_width();
    let out = ui.allocate_ui_with_layout(
        Vec2::new(width, CHROME_ROW_H),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().button_padding = Vec2::new(8.0, 2.0);
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 0.0);
            egui::Frame::NONE
                .fill(fill)
                .inner_margin(egui::Margin {
                    left: CHROME_PAD_X,
                    right: CHROME_PAD_X,
                    top: CHROME_PAD_Y,
                    bottom: CHROME_PAD_Y,
                })
                .show(ui, |ui| {
                    ui.set_min_size(Vec2::new(ui.available_width(), CHROME_CTRL_H));
                    ui.set_max_height(CHROME_CTRL_H);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.set_min_height(CHROME_CTRL_H);
                        add_contents(ui);
                    });
                });
        },
    );
    if draw_bottom_rule {
        let r = out.response.rect;
        ui.painter().hline(
            r.x_range(),
            r.bottom() - 0.5,
            egui::Stroke::new(1.0, t.border),
        );
    }
    out.response
}

/// Ghost chrome button — Media Assist / Story dock (`>`, `[`, Export XML, Pokrivalice…).
pub fn transport_btn(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let t = current(ui);
    let width = if label == "Export XML" { 100.0 } else { 40.0 };
    ui.add(
        egui::Button::new(RichText::new(label).color(t.text).monospace().size(FONT_UI))
            .min_size(Vec2::new(width, CHROME_CTRL_H))
            .fill(Color32::TRANSPARENT)
            .stroke(egui::Stroke::new(1.0, t.border)),
    )
}

/// Same ghost style for text actions (Spremi virtualni kadar, Očisti, Odustani…).
pub fn action_btn(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let t = current(ui);
    ui.add(
        egui::Button::new(RichText::new(label).color(t.text).size(FONT_UI))
            .min_size(Vec2::new(0.0, CHROME_CTRL_H))
            .fill(Color32::TRANSPARENT)
            .stroke(egui::Stroke::new(1.0, t.border)),
    )
}

/// Filled accent CTA (Uvezi, U redu) — only primary actions.
pub fn primary_btn(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let t = current(ui);
    ui.add(
        egui::Button::new(
            RichText::new(label)
                .color(Color32::WHITE)
                .strong()
                .size(FONT_UI),
        )
        .min_size(Vec2::new(0.0, CHROME_CTRL_H))
        .fill(t.accent),
    )
}

/// Text tab link — Media Assist `All` / `Virtual` / `Segment` (underline when active).
pub fn link_tab(ui: &mut egui::Ui, label: &str, active: bool) -> egui::Response {
    let t = current(ui);
    let text = if active {
        RichText::new(label).color(t.text).strong().size(FONT_UI)
    } else {
        RichText::new(label).color(t.muted).size(FONT_UI)
    };
    let resp = ui.add(
        egui::Label::new(text)
            .sense(Sense::click())
            .selectable(false),
    );
    if active {
        let r = resp.rect;
        ui.painter().hline(
            r.left()..=r.right(),
            r.bottom() + 1.0,
            egui::Stroke::new(2.0, t.accent),
        );
    }
    resp
}

/// Compact clickable text (Gore, Diskovi, tree rows) — no button chrome.
pub fn text_link(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    let t = current(ui);
    let color = if enabled { t.text } else { t.muted };
    ui.add_enabled(
        enabled,
        egui::Label::new(RichText::new(label).size(FONT_UI).color(color))
            .sense(Sense::click())
            .selectable(false),
    )
}

/// Shared IN/OUT/Trajanje label (Story dock + Ingest under-grid).
pub fn timecode_label(ui: &mut egui::Ui, label: &str, value: &str, focused: bool) {
    let t = current(ui);
    let label_color = if focused { t.focus } else { t.muted };
    let value_color = if focused { t.focus } else { t.tc_gold };
    ui.label(RichText::new(label).size(FONT_TC).color(label_color));
    ui.label(
        RichText::new(value)
            .monospace()
            .strong()
            .size(FONT_TC)
            .color(value_color),
    );
}

pub fn truncate(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}
