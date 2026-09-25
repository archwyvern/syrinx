//! syrinx-player: a player for syrinx sound sources.

// A GUI subsystem executable on Windows, so a double-click opens no console window beside the
// player. Debug builds keep the console for their stderr.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod cache;
mod fader;
mod instance;
mod mixer;
mod output;
mod playlist;
mod register;
mod resample;
mod session;
mod theme;
mod track;
mod watch;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;

use crate::instance::{Acquire, Request};

/// A first launch's window, in points: room for a dozen layers without scrolling.
const DEFAULT_WINDOW: [f32; 2] = [880.0, 800.0];

#[derive(Parser)]
#[command(
    name = "syrinx-player",
    version,
    about = "Plays syrinx sound sources: a playlist, a fader per layer, rendering as you listen."
)]
struct Cli {
    /// Windows: associate .syr with this executable for the current user, then exit.
    #[arg(long)]
    register: bool,
    /// Windows: remove the .syr association, then exit.
    #[arg(long)]
    unregister: bool,
    /// Write a PNG of the window two seconds after it opens, then exit. For checking the
    /// window by eye on a desktop that blocks screenshots of Wayland windows.
    #[arg(long, hide = true, value_name = "PNG")]
    screenshot: Option<PathBuf>,
    /// Seconds to wait before the screenshot.
    #[arg(long, hide = true, default_value_t = 2.0, value_name = "SECONDS")]
    screenshot_delay: f64,
    /// The window's size in points for a screenshot run, `WIDTHxHEIGHT`, to check a narrow or a
    /// tall window; the default is what a first launch gets.
    #[arg(long, hide = true, value_name = "WxH")]
    window_size: Option<String>,
    /// Name of the single-instance socket to use instead of the user's, so a test can run its
    /// own player beside a real one. A screenshot run gets a private one by default.
    #[arg(long, hide = true, value_name = "NAME")]
    instance: Option<String>,
    /// Sources (.syr) or folders of them. They replace the playlist of a running player.
    paths: Vec<PathBuf>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if cli.register {
        let exe = std::env::current_exe().context("locating this executable")?;
        return register::register(&exe);
    }
    if cli.unregister {
        return register::unregister();
    }

    let paths: Vec<PathBuf> = cli
        .paths
        .iter()
        .map(|p| {
            if p.is_absolute() {
                p.clone()
            } else {
                std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.clone())
            }
        })
        .collect();
    // A screenshot run is its own process, never handed to a running player.
    let suffix = match &cli.instance {
        Some(name) => format!("-{name}"),
        None if cli.screenshot.is_some() => format!("-shot-{}", std::process::id()),
        None => String::new(),
    };
    let rx = match instance::acquire(&suffix, Request { replace: !paths.is_empty(), paths: paths.clone() }) {
        Acquire::Handled => return Ok(()),
        Acquire::Listening(rx) => rx,
    };

    let cache = cache::Cache::open()?;
    if let Err(e) = cache.evict(cache::CACHE_CAP_BYTES, &[]) {
        eprintln!("warning: trimming the render cache: {e:#}");
    }

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../../logo/syrinx-512.png"))
        .context("decoding the window icon")?;
    // A screenshot run is silent and keeps its window and settings to itself, so checking the
    // window by eye never plays through the user's speakers or changes the user's player.
    let ephemeral = cli.screenshot.is_some();
    let size = match &cli.window_size {
        Some(spec) => {
            let (w, h) = spec.split_once('x').context("--window-size wants WIDTHxHEIGHT")?;
            [
                w.trim().parse::<f32>().context("--window-size width")?,
                h.trim().parse::<f32>().context("--window-size height")?,
            ]
        }
        None => DEFAULT_WINDOW,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("syrinx-player")
            .with_app_id("syrinx-player")
            .with_inner_size(size)
            .with_min_inner_size([580.0, 380.0])
            .with_icon(icon),
        persist_window: !ephemeral,
        persistence_path: ephemeral.then(|| std::env::temp_dir().join("syrinx-player-screenshot")),
        ..Default::default()
    };
    eframe::run_native(
        "syrinx-player",
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            let mut app = app::PlayerApp::new(cc, cache, paths, rx, ephemeral);
            app.screenshot_to(cli.screenshot, std::time::Duration::from_secs_f64(cli.screenshot_delay));
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}
