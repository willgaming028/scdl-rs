//! scdl-gui — the animated desktop front-end.
//!
//! Shares `scdl-core` with the CLI/TUI, so all three behave identically; only
//! the presentation differs.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anim;
mod app;
mod bridge;
mod icons;
mod state;
mod theme;

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "scdl-gui",
    version,
    about = "Animated desktop UI for the SoundCloud downloader"
)]
struct Args {
    /// Directory to download into
    #[arg(long, value_name = "PATH")]
    path: Option<PathBuf>,

    /// Fill the UI with fake data — no network, useful for a look around
    #[arg(long)]
    demo: bool,

    /// Render one frame to a PNG and exit. Implies --demo.
    #[arg(long, value_name = "FILE")]
    screenshot: Option<PathBuf>,

    /// Window size for --screenshot, as WIDTHxHEIGHT
    #[arg(long, default_value = "1440x900", value_name = "WxH")]
    size: String,

    /// Which view to open on: browse, queue, library, settings
    #[arg(long, default_value = "browse")]
    view: String,
}

fn parse_size(s: &str) -> (f32, f32) {
    let mut it = s.split(['x', 'X']);
    let w = it.next().and_then(|v| v.trim().parse::<f32>().ok());
    let h = it.next().and_then(|v| v.trim().parse::<f32>().ok());
    match (w, h) {
        (Some(w), Some(h)) if w >= 320.0 && h >= 240.0 => (w, h),
        _ => (1440.0, 900.0),
    }
}

fn parse_view(s: &str) -> state::View {
    match s.trim().to_ascii_lowercase().as_str() {
        "queue" => state::View::Queue,
        "library" => state::View::Library,
        "settings" => state::View::Settings,
        _ => state::View::Browse,
    }
}

fn main() -> eframe::Result<()> {
    let args = Args::parse();
    let (w, h) = parse_size(&args.size);
    let demo = args.demo || args.screenshot.is_some();

    let output_dir = args.path.clone().unwrap_or_else(|| {
        scdl_core::config::Config::load(&scdl_core::config::default_config_path())
            .map(|c| c.path)
            .unwrap_or_else(|_| PathBuf::from("."))
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([w, h])
            .with_min_inner_size([900.0, 560.0])
            .with_title("scdl")
            .with_app_id("scdl"),
        // 4x MSAA: the app draws a lot of arcs and diagonal lines by hand, and
        // they alias badly without it.
        multisampling: 4,
        ..Default::default()
    };

    let screenshot = args.screenshot.clone();
    let view = parse_view(&args.view);

    eframe::run_native(
        "scdl",
        options,
        Box::new(move |cc| {
            let mut app = app::ScdlApp::new(cc, output_dir, demo);
            app.screenshot = screenshot;
            app.state.view = view;
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_parsing_accepts_sensible_input() {
        assert_eq!(parse_size("1920x1080"), (1920.0, 1080.0));
        assert_eq!(parse_size("1280X720"), (1280.0, 720.0));
    }

    #[test]
    fn size_parsing_falls_back_on_nonsense() {
        assert_eq!(parse_size(""), (1440.0, 900.0));
        assert_eq!(parse_size("banana"), (1440.0, 900.0));
        assert_eq!(parse_size("10x10"), (1440.0, 900.0), "too small");
        assert_eq!(parse_size("-5x-5"), (1440.0, 900.0));
    }

    #[test]
    fn view_parsing_is_case_insensitive_with_a_default() {
        assert_eq!(parse_view("Queue"), state::View::Queue);
        assert_eq!(parse_view("  settings "), state::View::Settings);
        assert_eq!(parse_view("nonsense"), state::View::Browse);
    }
}
