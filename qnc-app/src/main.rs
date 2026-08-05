//! QNC native desktop app — egui UI against qnc-host (no WebView / HTML).

mod api;
mod app;
mod carrier_sync;
mod composition;
mod editorial;
mod frame_time;
mod ingest;
mod ingest_player;
mod media_assist;
mod playback_stack;
mod player_bridge;
mod player_contract;
mod player_log;
mod player_remote;
mod project;
mod project_pts;
mod qnc_broadcast_player;
mod qnc_filmstrip_background;
mod qnc_form;
mod qnc_location_browser;
mod qnc_media_card;
mod qnc_source_dock;
mod qnc_theme;
mod qnc_timeline;
mod qnc_timeline_progress;
mod qnc_ui;
mod shortcuts;
mod story;

use clap::Parser;
use eframe::egui;

use app::QncApp;

#[derive(Debug, Parser)]
#[command(name = "qnc-app", about = "QNC native app (Project / Ingest / Story)")]
struct Cli {
    /// qnc-host base URL
    #[arg(long, env = "QNC_HOST_URL", default_value = "http://127.0.0.1:8001")]
    host: String,
}

fn main() -> eframe::Result<()> {
    let cli = Cli::parse();
    let host = cli.host.trim_end_matches('/').to_string();

    // Cross-platform (Win / macOS / Linux): start maximized via ViewportBuilder only.
    // Do not combine with_inner_size + with_maximized — on Windows egui/winit can
    // report maximized while keeping the restored rectangle (footer clipped, side gaps).
    // See emilk/egui#8243. Same builder path is used on all three OS targets.
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_min_inner_size([1100.0, 700.0])
            .with_maximized(true)
            .with_title("QNC App"),
        persist_window: false,
        ..Default::default()
    };

    eframe::run_native(
        "QNC App",
        options,
        Box::new(move |cc| {
            crate::qnc_theme::apply_app_fonts(&cc.egui_ctx);
            let tokens = crate::qnc_theme::ThemeId::Dark.tokens();
            crate::qnc_theme::set_active(&cc.egui_ctx, crate::qnc_theme::ThemeId::Dark);
            crate::qnc_theme::apply_egui_visuals(&cc.egui_ctx, &tokens);
            Ok(Box::new(QncApp::new(host)))
        }),
    )
}
