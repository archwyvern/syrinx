//! The window: a playlist on the left; the track's name and geometry, the seek bar (a picture
//! of the sound with the unrendered tail hatched), a fader per layer, the transport and a
//! status line on the right. Everything that takes time happens on another thread; this file
//! reads atomics and channels once per frame.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use egui::{Color32, Key, Modifiers, RichText, Sense, Stroke, Vec2, ViewportCommand};
use egui_file_dialog::FileDialog;
use serde::{Deserialize, Serialize};

use crate::cache::Cache;
use crate::instance::Request;
use crate::mixer::{Command, Gains, MixerHandle};
use crate::output::{DeviceInfo, Output, Shared, devices};
use crate::playlist::{self, Inspected, Playlist};
use crate::theme;
use crate::track::{Render, Track};
use crate::watch::Watch;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Settings {
    pub volume: f32,
    pub loop_track: bool,
    /// `None` = the default device.
    pub device: Option<String>,
    pub reload_on_change: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { volume: 0.8, loop_track: false, device: None, reload_on_change: true }
    }
}

const SETTINGS_KEY: &str = "settings";
const SEEK_SMALL: f64 = 5.0;
const SEEK_LARGE: f64 = 30.0;

pub struct PlayerApp {
    cache: Cache,
    settings: Settings,
    playlist: Playlist,
    inspects: Vec<Receiver<Inspected>>,
    requests: Receiver<Request>,
    shared: Arc<Shared>,
    output: Option<Output>,
    output_error: Arc<Mutex<Option<String>>>,
    mixer: MixerHandle,
    track: Option<Arc<Track>>,
    gains: Option<Arc<Gains>>,
    watch: Option<Watch>,
    dialog: FileDialog,
    selected: Option<usize>,
    devices: Vec<DeviceInfo>,
    devices_listed: Instant,
    note: Option<(String, Instant)>,
    /// Where the playhead is meant to be while no chunk has been pushed since a seek.
    seek_target: Option<usize>,
    /// `--screenshot`: where to write the window, when it was opened, the delay, whether asked yet.
    screenshot: Option<(PathBuf, Instant, Duration, bool)>,
    ctx: egui::Context,
}

impl PlayerApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cache: Cache,
        initial: Vec<PathBuf>,
        requests: Receiver<Request>,
    ) -> Result<PlayerApp> {
        let settings: Settings = cc.storage.and_then(|s| eframe::get_value(s, SETTINGS_KEY)).unwrap_or_default();
        let shared = Shared::new(settings.volume);
        let output_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let ctx = cc.egui_ctx.clone();

        let (output, producer, note) = open_output(settings.device.as_deref(), &shared, &output_error, &ctx);
        let (rate, channels) = output.as_ref().map_or((48_000, 2), |o| (o.sample_rate, o.channels));
        let mixer = MixerHandle::spawn(producer, Arc::clone(&shared), rate, channels);
        mixer.send(Command::Loop(settings.loop_track));

        let mut app = PlayerApp {
            cache,
            settings,
            playlist: Playlist::default(),
            inspects: Vec::new(),
            requests,
            shared,
            output,
            output_error,
            mixer,
            track: None,
            gains: None,
            watch: None,
            dialog: FileDialog::new()
                .title("Add sources")
                .add_file_filter_extensions("syrinx sources", vec!["syr"])
                .default_file_filter("syrinx sources"),
            selected: None,
            devices: Vec::new(),
            devices_listed: Instant::now().checked_sub(Duration::from_secs(60)).unwrap_or_else(Instant::now),
            note: note.map(|n| (n, Instant::now())),
            seek_target: None,
            screenshot: None,
            ctx,
        };
        if !initial.is_empty() {
            let added = app.playlist.replace(&initial);
            app.queue_inspect(added);
            app.play(0);
        }
        Ok(app)
    }

    pub fn screenshot_to(&mut self, path: Option<PathBuf>, delay: Duration) {
        self.screenshot = path.map(|p| (p, Instant::now(), delay, false));
    }

    /// The `--screenshot` flow: ask for the frame once the delay is up, write it, close.
    fn poll_screenshot(&mut self) {
        let Some((path, opened, delay, asked)) = self.screenshot.clone() else {
            return;
        };
        if !asked {
            if opened.elapsed() > delay {
                self.ctx.send_viewport_cmd(ViewportCommand::Screenshot(egui::UserData::default()));
                self.screenshot = Some((path, opened, delay, true));
            }
            self.ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        let image = self.ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(Arc::clone(image)),
                _ => None,
            })
        });
        if let Some(image) = image {
            let (w, h) = (image.width() as u32, image.height() as u32);
            match image::RgbaImage::from_raw(w, h, image.as_raw().to_vec()) {
                Some(rgba) => match rgba.save(&path) {
                    Ok(()) => eprintln!("wrote {}", path.display()),
                    Err(e) => eprintln!("error: writing {}: {e}", path.display()),
                },
                None => eprintln!("error: screenshot of {w}x{h} has the wrong byte count"),
            }
            self.ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    fn repaint(&self) -> impl Fn() + Send + Sync + 'static {
        let ctx = self.ctx.clone();
        move || ctx.request_repaint()
    }

    fn say(&mut self, text: impl Into<String>) {
        self.note = Some((text.into(), Instant::now()));
    }

    // ------------------------------------------------------------------ playlist and tracks

    fn queue_inspect(&mut self, indices: Vec<usize>) {
        if indices.is_empty() {
            return;
        }
        let rows: Vec<(usize, PathBuf)> = indices.iter().map(|&i| (i, self.playlist.rows[i].path.clone())).collect();
        let rx = playlist::inspect_rows(rows, self.repaint());
        self.inspects.push(rx);
    }

    fn poll_inspects(&mut self) {
        let mut done = Vec::new();
        for (k, rx) in self.inspects.iter().enumerate() {
            loop {
                match rx.try_recv() {
                    Ok(found) => {
                        // Rows may have moved since the inspection was queued: match by path.
                        if let Some(row) = self.playlist.rows.iter_mut().find(|r| r.path == found.path) {
                            match found.result {
                                Ok((name, duration, stems)) => {
                                    // A source that declares no name keeps its file's stem.
                                    if let Some(name) = name {
                                        row.name = name;
                                    }
                                    row.duration = Some(duration);
                                    row.stems = stems;
                                    row.error = None;
                                }
                                Err(e) => row.error = Some(e),
                            }
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        done.push(k);
                        break;
                    }
                }
            }
        }
        for k in done.into_iter().rev() {
            self.inspects.remove(k);
        }
    }

    fn add_paths(&mut self, paths: &[PathBuf], replace: bool) {
        let added = if replace { self.playlist.replace(paths) } else { self.playlist.append(paths) };
        let first = added.first().copied();
        self.queue_inspect(added);
        if replace {
            self.stop();
            if let Some(i) = first {
                self.play(i);
            }
        } else if self.track.is_none() {
            if let Some(i) = first {
                self.play(i);
            }
        }
    }

    /// Loads row `i` and plays it from the start; a row that fails is marked and skipped, so a
    /// playlist plays through around a broken file.
    fn play(&mut self, i: usize) {
        let mut i = i;
        for _ in 0..self.playlist.rows.len().max(1) {
            if i >= self.playlist.rows.len() {
                return;
            }
            match self.load(i, 0) {
                Ok(()) => return,
                Err(e) => {
                    self.playlist.rows[i].error = Some(format!("{e:#}"));
                    i += 1;
                }
            }
        }
        self.stop();
    }

    fn load(&mut self, i: usize, start_frame: usize) -> Result<()> {
        let path = self.playlist.rows[i].path.clone();
        let track = Track::open(&self.cache, &path, self.repaint())?;
        // Faders survive a reload of the same file, by layer name.
        let previous = self.gains.take().zip(self.track.take());
        let gains = Arc::new(Gains::new(track.stem_names.len()));
        if let Some((old_gains, old_track)) = previous {
            if old_track.path == track.path {
                for (n, name) in track.stem_names.iter().enumerate() {
                    if let Some(o) = old_track.stem_names.iter().position(|s| s == name) {
                        gains.set_gain(n, old_gains.gain(o));
                        gains.set_mute(n, old_gains.muted(o));
                        gains.set_solo(n, old_gains.soloed(o));
                    }
                }
            }
        }
        self.watch = if self.settings.reload_on_change {
            let mut closure = track.dependencies.clone();
            closure.push(track.path.clone());
            match Watch::new(closure, self.repaint()) {
                Ok(w) => Some(w),
                Err(e) => {
                    self.say(format!("not watching for changes: {e:#}"));
                    None
                }
            }
        } else {
            None
        };
        // The mixer resets these too, but not before its thread gets to the command; until then
        // the previous track's end would read as this one's and advance the playlist again.
        self.shared.finished_at.store(u64::MAX, Ordering::Relaxed);
        self.mixer.status.at_end.store(false, Ordering::Relaxed);
        self.mixer.send(Command::Load { track: Arc::clone(&track), gains: Arc::clone(&gains), start_frame });
        self.shared.playing.store(true, Ordering::Relaxed);
        self.seek_target = Some(start_frame);
        self.ctx.send_viewport_cmd(ViewportCommand::Title(format!("{} - syrinx-player", track.name)));
        self.playlist.rows[i].name = track.name.clone();
        self.playlist.rows[i].duration = Some(track.duration);
        self.playlist.rows[i].stems = track.stem_names.clone();
        self.playlist.rows[i].error = None;
        self.playlist.current = Some(i);
        self.selected = Some(i);
        self.track = Some(track);
        self.gains = Some(gains);
        Ok(())
    }

    fn stop(&mut self) {
        self.shared.playing.store(false, Ordering::Relaxed);
        self.mixer.send(Command::Unload);
        self.track = None;
        self.gains = None;
        self.watch = None;
        self.seek_target = None;
        self.playlist.current = None;
        self.ctx.send_viewport_cmd(ViewportCommand::Title("syrinx-player".into()));
    }

    fn reload(&mut self) {
        let Some(i) = self.playlist.current else {
            return;
        };
        let position = self.position().unwrap_or(0);
        let was_playing = self.shared.playing.load(Ordering::Relaxed);
        eprintln!(
            "reload: {} changed, resuming at {}",
            self.playlist.rows[i].path.display(),
            fmt_time(position as f64 / self.track.as_ref().map_or(48_000.0, |t| t.sample_rate as f64))
        );
        if let Err(e) = self.load(i, position) {
            self.playlist.rows[i].error = Some(format!("{e:#}"));
            self.say(format!("{e:#}"));
            self.shared.playing.store(false, Ordering::Relaxed);
            return;
        }
        self.shared.playing.store(was_playing, Ordering::Relaxed);
    }

    fn rerender(&mut self) {
        let Some(track) = self.track.clone() else {
            return;
        };
        let position = self.position().unwrap_or(0);
        let i = self.playlist.current;
        // Drop every handle to the files before removing the directory.
        self.mixer.send(Command::Unload);
        self.track = None;
        let dir = track.dir.clone();
        drop(track);
        let _ = std::fs::remove_dir_all(&dir);
        if let Some(i) = i {
            if let Err(e) = self.load(i, position) {
                self.playlist.rows[i].error = Some(format!("{e:#}"));
            }
        }
    }

    fn toggle_play(&mut self) {
        if self.track.is_some() {
            let at_end = self.mixer.status.at_end.load(Ordering::Relaxed);
            if at_end && !self.settings.loop_track {
                self.seek(0);
                self.shared.playing.store(true, Ordering::Relaxed);
            } else {
                self.shared.playing.fetch_xor(true, Ordering::Relaxed);
            }
        } else if let Some(i) = self.selected.or_else(|| self.playlist.rows.first().map(|_| 0)) {
            self.play(i);
        }
    }

    fn seek(&mut self, frame: usize) {
        if let Some(track) = &self.track {
            let frame = frame.min(track.frames);
            self.shared.finished_at.store(u64::MAX, Ordering::Relaxed);
            self.mixer.status.at_end.store(false, Ordering::Relaxed);
            self.mixer.send(Command::Seek(frame));
            self.seek_target = Some(frame);
        }
    }

    fn seek_by(&mut self, seconds: f64) {
        if let Some(track) = &self.track {
            let now = self.position().unwrap_or(0) as f64;
            let target = (now + seconds * track.sample_rate as f64).max(0.0) as usize;
            self.seek(target);
        }
    }

    /// The source frame under the playhead: from the callback's frame count through the
    /// mixer's checkpoints, or the seek target while nothing has been pushed since a seek.
    fn position(&self) -> Option<usize> {
        let track = self.track.as_ref()?;
        let consumed = self.shared.consumed.load(Ordering::Relaxed);
        let device_rate = self.output.as_ref().map_or(track.sample_rate, |o| o.sample_rate);
        let mapped = self.mixer.checkpoints.position(consumed, device_rate, track.sample_rate);
        match (mapped, self.seek_target) {
            (Some(p), _) => Some(p.min(track.frames)),
            (None, Some(t)) => Some(t),
            (None, None) => Some(0),
        }
    }

    fn set_loop(&mut self, on: bool) {
        self.settings.loop_track = on;
        self.mixer.send(Command::Loop(on));
    }

    fn switch_device(&mut self, name: Option<String>) {
        let (output, producer, note) = open_output(name.as_deref(), &self.shared, &self.output_error, &self.ctx);
        if let Some(out) = &output {
            self.mixer.send(Command::Output { producer, sample_rate: out.sample_rate, channels: out.channels });
            self.settings.device = name;
        } else {
            // Nothing opened: keep the old stream and say why.
            drop(producer);
        }
        if let Some(n) = note {
            self.say(n);
        }
        if output.is_some() {
            self.output = output;
        }
    }

    // ------------------------------------------------------------------ per-frame events

    fn poll(&mut self) {
        self.poll_inspects();

        loop {
            match self.requests.try_recv() {
                Ok(request) => {
                    self.add_paths(&request.paths, request.replace);
                    self.ctx.send_viewport_cmd(ViewportCommand::Focus);
                }
                Err(_) => break,
            }
        }

        let device_error = self.output_error.lock().unwrap().take();
        if let Some(e) = device_error {
            self.say(format!("audio device: {e}"));
            self.output = None;
            self.switch_device(None);
        }

        if let Some(watch) = &self.watch {
            if watch.changed() {
                if self.settings.reload_on_change {
                    self.reload();
                }
            } else if watch.pending() {
                self.ctx.request_repaint_after(crate::watch::DEBOUNCE);
            }
        }

        // A track that ended (or died) while not looping: advance.
        if let Some(track) = self.track.clone() {
            let at_end = self.mixer.status.at_end.load(Ordering::Relaxed);
            let finished_at = self.shared.finished_at.load(Ordering::Relaxed);
            let consumed = self.shared.consumed.load(Ordering::Relaxed);
            if at_end && consumed >= finished_at && !self.settings.loop_track {
                let error = self.mixer.status.error.lock().unwrap().clone().or_else(|| track.failed());
                if let (Some(i), Some(e)) = (self.playlist.current, error) {
                    self.playlist.rows[i].error = Some(e);
                }
                match self.playlist.next() {
                    Some(next) => self.play(next),
                    None => {
                        self.shared.playing.store(false, Ordering::Relaxed);
                        self.mixer.send(Command::Seek(0));
                        self.seek_target = Some(0);
                    }
                }
            }
        }

        if let Some((_, since)) = &self.note {
            if since.elapsed() > Duration::from_secs(8) {
                self.note = None;
            }
        }
    }

    fn handle_keys(&mut self) {
        if self.ctx.egui_wants_keyboard_input() {
            return;
        }
        let ctx = self.ctx.clone();
        let pressed = |key: Key, modifiers: Modifiers| ctx.input_mut(|i| i.consume_key(modifiers, key));
        if pressed(Key::Space, Modifiers::NONE) {
            self.toggle_play();
        }
        if pressed(Key::Home, Modifiers::NONE) {
            self.seek(0);
        }
        if pressed(Key::ArrowLeft, Modifiers::SHIFT) {
            self.seek_by(-SEEK_LARGE);
        } else if pressed(Key::ArrowLeft, Modifiers::NONE) {
            self.seek_by(-SEEK_SMALL);
        }
        if pressed(Key::ArrowRight, Modifiers::SHIFT) {
            self.seek_by(SEEK_LARGE);
        } else if pressed(Key::ArrowRight, Modifiers::NONE) {
            self.seek_by(SEEK_SMALL);
        }
        if pressed(Key::ArrowUp, Modifiers::NONE) {
            let v = (self.shared.volume() + 0.05).min(1.0);
            self.shared.set_volume(v);
            self.settings.volume = v;
        }
        if pressed(Key::ArrowDown, Modifiers::NONE) {
            let v = (self.shared.volume() - 0.05).max(0.0);
            self.shared.set_volume(v);
            self.settings.volume = v;
        }
        if pressed(Key::L, Modifiers::NONE) {
            let on = !self.settings.loop_track;
            self.set_loop(on);
        }
        if pressed(Key::N, Modifiers::NONE) {
            if let Some(next) = self.playlist.next() {
                self.play(next);
            }
        }
        if pressed(Key::P, Modifiers::NONE) {
            if let Some(prev) = self.playlist.prev() {
                self.play(prev);
            }
        }
        if pressed(Key::Delete, Modifiers::NONE) {
            if let Some(i) = self.selected {
                self.remove_row(i);
            }
        }
        if pressed(Key::O, Modifiers::NONE) {
            self.dialog.pick_multiple();
        }
        if pressed(Key::R, Modifiers::NONE) {
            self.rerender();
        }
        if pressed(Key::Enter, Modifiers::NONE) {
            if let Some(i) = self.selected {
                self.play(i);
            }
        }
    }

    fn remove_row(&mut self, i: usize) {
        if i >= self.playlist.rows.len() {
            return;
        }
        if self.playlist.current == Some(i) {
            self.stop();
        }
        self.playlist.remove(i);
        self.selected = if self.playlist.rows.is_empty() { None } else { Some(i.min(self.playlist.rows.len() - 1)) };
    }

    fn handle_drops(&mut self) {
        let dropped: Vec<PathBuf> =
            self.ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            self.add_paths(&dropped, false);
        }
    }

    // ------------------------------------------------------------------ drawing

    fn playlist_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("playlist").color(theme::LABEL));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("+").on_hover_text("Add files or folders (O)").clicked() {
                    self.dialog.pick_multiple();
                }
                if !self.playlist.rows.is_empty() && ui.button("clear").clicked() {
                    self.stop();
                    self.playlist.clear();
                    self.selected = None;
                }
            });
        });
        ui.separator();
        if self.playlist.rows.is_empty() {
            ui.add_space(12.0);
            ui.label(RichText::new("Drop .syr files or folders here,\nor press + to add them.").color(theme::LABEL));
            return;
        }
        let mut play_row: Option<usize> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for i in 0..self.playlist.rows.len() {
                let row = &self.playlist.rows[i];
                let is_current = self.playlist.current == Some(i);
                let is_selected = self.selected == Some(i);
                let name = if row.error.is_some() {
                    RichText::new(&row.name).color(theme::ERROR)
                } else if is_current {
                    RichText::new(&row.name).color(theme::ACCENT).strong()
                } else {
                    RichText::new(&row.name)
                };
                let duration = row.duration.map(fmt_time).unwrap_or_default();
                let response = ui
                    .horizontal(|ui| {
                        let marker = if is_current {
                            if self.shared.playing.load(Ordering::Relaxed) { "\u{25b6}" } else { "\u{23f8}" }
                        } else {
                            " "
                        };
                        ui.add_sized([14.0, 18.0], egui::Label::new(RichText::new(marker).color(theme::ACCENT)));
                        let r = ui.selectable_label(is_selected, name);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(duration).color(theme::LABEL));
                        });
                        r
                    })
                    .inner;
                let response = match &row.error {
                    Some(e) => response.on_hover_text(e),
                    None => response.on_hover_text(row.path.display().to_string()),
                };
                if response.clicked() {
                    self.selected = Some(i);
                    play_row = Some(i);
                }
            }
        });
        if let Some(i) = play_row {
            self.play(i);
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        match &self.track {
            Some(track) => {
                ui.horizontal(|ui| {
                    ui.heading(&track.name);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let geometry = format!(
                            "{}  {} Hz  {}",
                            if track.channels == 1 { "mono" } else { "stereo" },
                            track.sample_rate,
                            fmt_time(track.duration)
                        );
                        ui.label(RichText::new(geometry).color(theme::LABEL));
                    });
                });
            }
            None => {
                ui.heading(RichText::new("nothing loaded").color(theme::LABEL));
            }
        }
    }

    fn overview(&mut self, ui: &mut egui::Ui) {
        let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 84.0), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, theme::WAVE_BG);
        let Some(track) = self.track.clone() else {
            return;
        };
        let overview = track.overview.lock().unwrap().clone();
        let total_columns = track.frames.div_ceil(overview.frames_per_column.max(1)).max(1);
        let column_width = rect.width() / total_columns as f32;
        let mid = rect.center().y;
        let half = rect.height() * 0.5 - 4.0;
        for (c, (lo, hi)) in overview.columns.iter().enumerate().take(overview.ready) {
            let x = rect.left() + (c as f32 + 0.5) * column_width;
            let y0 = mid - hi.clamp(-1.0, 1.0) * half;
            let y1 = mid - lo.clamp(-1.0, 1.0) * half;
            painter.line_segment(
                [egui::pos2(x, y0.min(mid - 0.5)), egui::pos2(x, y1.max(mid + 0.5))],
                Stroke::new(column_width.max(1.0), theme::WAVE),
            );
        }
        // The unrendered tail: hatched over, from the slowest layer's frontier to the end.
        let unity = self.gains.as_ref().is_none_or(|g| g.all_unity());
        let rendered_to = track.rendered_to(unity).unwrap_or(0);
        if rendered_to < track.frames {
            let x = rect.left() + rect.width() * rendered_to as f32 / track.frames as f32;
            let tail = egui::Rect::from_min_max(egui::pos2(x, rect.top()), rect.max);
            painter.rect_filled(tail, 0.0, theme::PENDING);
            let mut hx = x;
            while hx < rect.right() {
                painter.line_segment(
                    [egui::pos2(hx, rect.bottom()), egui::pos2((hx + rect.height()).min(rect.right()), rect.top())],
                    Stroke::new(1.0, Color32::from_gray(0x60)),
                );
                hx += 12.0;
            }
        }
        if let Some(position) = self.position() {
            let x = rect.left() + rect.width() * position as f32 / track.frames.max(1) as f32;
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                Stroke::new(2.0, theme::PLAYHEAD),
            );
        }
        if response.clicked() || response.dragged() {
            if let Some(pos) = response.interact_pointer_pos() {
                let fraction = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                self.seek((fraction as f64 * track.frames as f64) as usize);
            }
        }
    }

    fn faders(&mut self, ui: &mut egui::Ui) {
        let (Some(track), Some(gains)) = (self.track.clone(), self.gains.clone()) else {
            return;
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new("layers").color(theme::LABEL));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("reset").on_hover_text("Every fader to unity, no mute, no solo").clicked() {
                    gains.reset();
                }
            });
        });
        // One group is label + slider + dB + M + S; as many across as the width takes.
        let group_width = 110.0 + 140.0 + 64.0 + 2.0 * 26.0 + 5.0 * 10.0;
        let per_row = ((ui.available_width() / group_width).floor() as usize).clamp(1, 3);
        let per_row = if track.stem_names.len() <= 3 { 1 } else { per_row };
        egui::Grid::new("faders").num_columns(per_row * 5).spacing([10.0, 6.0]).show(ui, |ui| {
            for (i, name) in track.stem_names.iter().enumerate() {
                let mut gain = gains.gain(i);
                let mut muted = gains.muted(i);
                let mut soloed = gains.soloed(i);
                let label = if muted || (gains.effective()[i] == 0.0) {
                    RichText::new(name).color(theme::LABEL)
                } else {
                    RichText::new(name)
                };
                ui.add_sized([110.0, 20.0], egui::Label::new(label).truncate());
                let slider = egui::Slider::new(&mut gain, 0.0..=2.0).show_value(false);
                if ui.add(slider).changed() {
                    gains.set_gain(i, gain);
                }
                ui.add_sized([64.0, 20.0], egui::Label::new(RichText::new(fmt_db(gain)).monospace()));
                if ui.toggle_value(&mut muted, "M").on_hover_text("Mute").changed() {
                    gains.set_mute(i, muted);
                }
                if ui.toggle_value(&mut soloed, "S").on_hover_text("Solo").changed() {
                    gains.set_solo(i, soloed);
                }
                if (i + 1) % per_row == 0 || i + 1 == track.stem_names.len() {
                    ui.end_row();
                }
            }
        });
    }

    fn transport(&mut self, ui: &mut egui::Ui) {
        let has_track = self.track.is_some();
        let playing = self.shared.playing.load(Ordering::Relaxed);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.playlist.prev().is_some(), egui::Button::new("\u{23ee}"))
                .on_hover_text("Previous (P)")
                .clicked()
            {
                if let Some(prev) = self.playlist.prev() {
                    self.play(prev);
                }
            }
            let play_label = if playing && has_track { "\u{23f8}" } else { "\u{25b6}" };
            if ui
                .add_enabled(
                    has_track || !self.playlist.rows.is_empty(),
                    egui::Button::new(play_label).min_size(Vec2::new(40.0, 0.0)),
                )
                .on_hover_text("Play / pause (Space)")
                .clicked()
            {
                self.toggle_play();
            }
            if ui.add_enabled(has_track, egui::Button::new("\u{23f9}")).on_hover_text("Stop").clicked() {
                self.stop();
            }
            if ui
                .add_enabled(self.playlist.next().is_some(), egui::Button::new("\u{23ed}"))
                .on_hover_text("Next (N)")
                .clicked()
            {
                if let Some(next) = self.playlist.next() {
                    self.play(next);
                }
            }
            ui.add_space(8.0);
            let time = match &self.track {
                Some(track) => {
                    let position = self.position().unwrap_or(0) as f64 / track.sample_rate as f64;
                    format!("{} / {}", fmt_time(position), fmt_time(track.duration))
                }
                None => "-:--.- / -:--.-".into(),
            };
            ui.label(RichText::new(time).monospace());
            ui.add_space(8.0);
            let mut looping = self.settings.loop_track;
            if ui.toggle_value(&mut looping, "loop").on_hover_text("Repeat this track (L)").changed() {
                self.set_loop(looping);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut volume = self.shared.volume();
                if ui
                    .add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false))
                    .on_hover_text("Volume (Up / Down)")
                    .changed()
                {
                    self.shared.set_volume(volume);
                    self.settings.volume = volume;
                }
                ui.label(RichText::new("volume").color(theme::LABEL));
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (text, color) = self.status_text();
            ui.label(RichText::new(text).color(color));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("\u{2630}", |ui| {
                    if ui.button("Clear render cache").clicked() {
                        let dir = self.track.as_ref().map(|t| t.dir.clone());
                        match self.cache.clear() {
                            Ok(()) => self.say("render cache cleared"),
                            Err(e) => self.say(format!("clearing the cache: {e:#}")),
                        }
                        if dir.is_some() {
                            self.rerender();
                        }
                        ui.close();
                    }
                    if ui.button("Re-render this track (R)").clicked() {
                        self.rerender();
                        ui.close();
                    }
                    ui.label(
                        RichText::new(format!("cache: {}", self.cache.root().display())).color(theme::LABEL).small(),
                    );
                });
                let mut reload = self.settings.reload_on_change;
                if ui
                    .toggle_value(&mut reload, "reload on change")
                    .on_hover_text("Re-render when the source or an import changes")
                    .changed()
                {
                    self.settings.reload_on_change = reload;
                    if reload {
                        self.reload_watch();
                    } else {
                        self.watch = None;
                    }
                }
                self.device_picker(ui);
            });
        });
    }

    fn reload_watch(&mut self) {
        if let Some(track) = &self.track {
            let mut closure = track.dependencies.clone();
            closure.push(track.path.clone());
            self.watch = Watch::new(closure, self.repaint()).ok();
        }
    }

    fn device_picker(&mut self, ui: &mut egui::Ui) {
        let current = self.output.as_ref().map_or("no output device".to_string(), |o| o.device_name.clone());
        let mut pick: Option<Option<String>> = None;
        egui::ComboBox::from_id_salt("device").selected_text(current).width(220.0).show_ui(ui, |ui| {
            if self.devices_listed.elapsed() > Duration::from_secs(1) {
                self.devices = devices();
                self.devices_listed = Instant::now();
            }
            if ui.selectable_label(self.settings.device.is_none(), "default device").clicked() {
                pick = Some(None);
            }
            for d in &self.devices {
                let label = if d.is_default { format!("{} (default)", d.name) } else { d.name.clone() };
                if ui.selectable_label(self.settings.device.as_deref() == Some(d.name.as_str()), label).clicked() {
                    pick = Some(Some(d.name.clone()));
                }
            }
        });
        if let Some(choice) = pick {
            self.switch_device(choice);
        }
    }

    fn status_text(&self) -> (String, Color32) {
        if let Some((note, _)) = &self.note {
            return (note.clone(), theme::WARN);
        }
        let Some(track) = &self.track else {
            return (format!("{} in the playlist", self.playlist.rows.len()), theme::LABEL);
        };
        if let Some(e) = self.mixer.status.error.lock().unwrap().clone() {
            return (e, theme::ERROR);
        }
        let render = track.render_state();
        if let Render::Failed(e) = &render {
            return (e.clone(), theme::ERROR);
        }
        let mut parts: Vec<String> = Vec::new();
        match render {
            Render::Rendering { started } => {
                let frontier = track.stem_frontier();
                parts.push(format!(
                    "rendering {:.1} s, {} rendered",
                    started.elapsed().as_secs_f64(),
                    fmt_time(frontier as f64 / track.sample_rate as f64)
                ));
            }
            Render::Done { took } => parts.push(if took < Duration::from_millis(5) {
                "from cache".into()
            } else {
                format!("rendered in {:.1} s", took.as_secs_f64())
            }),
            Render::Failed(_) => {}
        }
        if self.mixer.status.waiting.load(Ordering::Relaxed) || self.shared.starved.load(Ordering::Relaxed) {
            parts.push("waiting for the render".into());
        }
        if self.mixer.status.master_bypassed.load(Ordering::Relaxed) {
            parts.push("master bypassed while faders are moved".into());
        }
        if let Some(out) = &self.output {
            if out.sample_rate != track.sample_rate {
                parts.push(format!("resampling to {} Hz", out.sample_rate));
            }
        }
        (parts.join("  \u{00b7}  "), theme::LABEL)
    }
}

impl eframe::App for PlayerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll();
        self.poll_screenshot();
        self.handle_drops();
        self.handle_keys();

        self.dialog.update(&self.ctx.clone());
        if let Some(paths) = self.dialog.take_picked_multiple() {
            self.add_paths(&paths, false);
        }

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.add_space(2.0);
            self.status_bar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::left("playlist").resizable(true).default_size(260.0).min_size(180.0).show(ui, |ui| {
            ui.add_space(4.0);
            self.playlist_panel(ui);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(4.0);
            self.header(ui);
            ui.add_space(6.0);
            self.overview(ui);
            ui.add_space(8.0);
            self.faders(ui);
            ui.add_space(10.0);
            self.transport(ui);
        });

        if self.track.is_some() {
            self.ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.settings);
    }
}

/// Opens the wanted device, falling back to the default, then to nothing; the third item is a
/// message for the status line when something was not as asked.
fn open_output(
    wanted: Option<&str>,
    shared: &Arc<Shared>,
    output_error: &Arc<Mutex<Option<String>>>,
    ctx: &egui::Context,
) -> (Option<Output>, rtrb::Producer<f32>, Option<String>) {
    let on_error = {
        let slot = Arc::clone(output_error);
        let ctx = ctx.clone();
        move |e: String| {
            *slot.lock().unwrap() = Some(e);
            ctx.request_repaint();
        }
    };
    match Output::open(wanted, Arc::clone(shared), on_error.clone()) {
        Ok((output, producer)) => (Some(output), producer, None),
        Err(first) => {
            if wanted.is_some() {
                if let Ok((output, producer)) = Output::open(None, Arc::clone(shared), on_error) {
                    return (Some(output), producer, Some(format!("{first:#}; using the default device")));
                }
            }
            let (producer, _consumer) = rtrb::RingBuffer::<f32>::new(1);
            (None, producer, Some(format!("no audio output: {first:#}")))
        }
    }
}

pub fn fmt_time(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    let minutes = (seconds / 60.0).floor() as u64;
    let rest = seconds - minutes as f64 * 60.0;
    format!("{minutes}:{rest:04.1}")
}

pub fn fmt_db(gain: f32) -> String {
    if gain <= 0.0005 { "-inf dB".into() } else { format!("{:+.1} dB", 20.0 * gain.log10()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_and_db_formats() {
        assert_eq!(fmt_time(0.0), "0:00.0");
        assert_eq!(fmt_time(62.45), "1:02.5");
        assert_eq!(fmt_time(268.0), "4:28.0");
        assert_eq!(fmt_db(1.0), "+0.0 dB");
        assert_eq!(fmt_db(2.0), "+6.0 dB");
        assert_eq!(fmt_db(0.5), "-6.0 dB");
        assert_eq!(fmt_db(0.0), "-inf dB");
    }
}
