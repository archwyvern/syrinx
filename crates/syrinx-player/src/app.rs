//! The window: a playlist on the left; the track's name and geometry, the seek bar (a picture
//! of the sound with the unrendered tail hatched), a fader per layer, the transport and a
//! status line on the right. It decides nothing about what plays: each frame it draws the
//! session's latest [`View`] and sends it [`Action`]s, so the player carries on while the window
//! goes unpainted (minimised, or on another workspace).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use egui::{Align, Color32, Key, Layout, Modifiers, Rect, RichText, Sense, Stroke, Vec2, ViewportCommand};
use egui_file_dialog::FileDialog;

use crate::cache::Cache;
use crate::fader::{self, Needle};
use crate::instance::Request;
use crate::mixer::Gains;
use crate::output::Cpal;
use crate::playlist::{Row, RowId};
use crate::session::{Action, SessionHandle, Settings, Start, View};
use crate::theme;
use crate::track::{Render, Track};

const SETTINGS_KEY: &str = "settings";
/// The keyboard, as the menu lists it.
const SHORTCUTS: &[(&str, &str)] = &[
    ("Space", "play / pause"),
    ("Enter", "play the selected track"),
    ("Left / Right", "seek 5 s; with Shift, 30 s"),
    ("Home", "back to the start"),
    ("Up / Down", "volume"),
    ("N / P", "next / previous track"),
    ("L", "loop this track"),
    ("O", "add files or folders"),
    ("Delete", "remove the selected track"),
    ("R", "re-render this track"),
];

const SEEK_SMALL: f64 = 5.0;
const SEEK_LARGE: f64 = 30.0;
/// How long a note stays in the status line.
const NOTE_FOR: Duration = Duration::from_secs(8);

pub struct PlayerApp {
    session: SessionHandle,
    cache_root: PathBuf,
    dialog: FileDialog,
    selected: Option<RowId>,
    /// The loaded row as of the last frame: the selection follows the track that plays.
    followed: Option<RowId>,
    /// The window title as last set.
    title: String,
    /// The session's hand-over count as of the last frame.
    handovers: u64,
    /// When the picker last asked for the device list.
    devices_asked: Option<Instant>,
    /// Where the seek bar is being dragged to, as a fraction; the seek happens on release.
    scrub: Option<f32>,
    /// The layers' meters, for the track they were made for, and when they last moved.
    needles: Vec<Needle>,
    needles_for: Option<usize>,
    needles_at: Instant,
    /// `--screenshot`: where to write the window, when it was opened, the delay, whether asked yet.
    screenshot: Option<(PathBuf, Instant, Duration, bool)>,
    /// A screenshot run: default settings, muted at the device, nothing saved.
    ephemeral: bool,
    ctx: egui::Context,
}

impl PlayerApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cache: Cache,
        initial: Vec<PathBuf>,
        requests: Receiver<Request>,
        ephemeral: bool,
    ) -> PlayerApp {
        let settings: Settings = if ephemeral {
            Settings::default()
        } else {
            cc.storage.and_then(|s| eframe::get_value(s, SETTINGS_KEY)).unwrap_or_default()
        };
        let ctx = cc.egui_ctx.clone();
        let cache_root = cache.root().to_path_buf();
        let repaint = {
            let ctx = ctx.clone();
            move || ctx.request_repaint()
        };
        let session = SessionHandle::spawn(
            Start { cache, settings, paths: initial, requests, muted: ephemeral },
            || Box::new(Cpal::default()),
            repaint,
        );
        PlayerApp {
            session,
            cache_root,
            dialog: FileDialog::new()
                .title("Add sources")
                .add_file_filter_extensions("syrinx sources", vec!["syr"])
                .default_file_filter("syrinx sources"),
            selected: None,
            followed: None,
            title: "syrinx-player".into(),
            handovers: 0,
            devices_asked: None,
            scrub: None,
            needles: Vec::new(),
            needles_for: None,
            needles_at: Instant::now(),
            screenshot: None,
            ephemeral,
            ctx,
        }
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

    fn send(&self, action: Action) {
        self.session.send(action);
    }

    // ------------------------------------------------------------------ per-frame events

    /// What the window does about the session's changes: the selection follows the loaded
    /// track, the title names it, and a hand-over from a later launch brings the window forward.
    fn follow(&mut self, view: &View) {
        let current = view.playlist.current.map(|i| view.playlist.rows[i].id);
        if current != self.followed {
            if current.is_some() {
                self.selected = current;
            }
            self.followed = current;
        }
        if let Some(id) = self.selected
            && view.playlist.index_of(id).is_none()
        {
            self.selected = None;
        }
        let title =
            view.track.as_ref().map_or_else(|| "syrinx-player".to_string(), |t| format!("{} - syrinx-player", t.name));
        if title != self.title {
            self.ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        if view.handovers != self.handovers {
            self.handovers = view.handovers;
            self.ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
    }

    fn handle_keys(&mut self, view: &View) {
        if self.ctx.egui_wants_keyboard_input() {
            return;
        }
        let ctx = self.ctx.clone();
        let pressed = |key: Key, modifiers: Modifiers| ctx.input_mut(|i| i.consume_key(modifiers, key));
        if pressed(Key::Space, Modifiers::NONE) {
            self.send(Action::TogglePlay { fallback: self.selected });
        }
        if pressed(Key::Home, Modifiers::NONE) {
            self.send(Action::Seek(0));
        }
        if pressed(Key::ArrowLeft, Modifiers::SHIFT) {
            self.send(Action::SeekBy(-SEEK_LARGE));
        } else if pressed(Key::ArrowLeft, Modifiers::NONE) {
            self.send(Action::SeekBy(-SEEK_SMALL));
        }
        if pressed(Key::ArrowRight, Modifiers::SHIFT) {
            self.send(Action::SeekBy(SEEK_LARGE));
        } else if pressed(Key::ArrowRight, Modifiers::NONE) {
            self.send(Action::SeekBy(SEEK_SMALL));
        }
        if pressed(Key::ArrowUp, Modifiers::NONE) {
            self.send(Action::SetVolume(view.shared.volume() + 0.05));
        }
        if pressed(Key::ArrowDown, Modifiers::NONE) {
            self.send(Action::SetVolume(view.shared.volume() - 0.05));
        }
        if pressed(Key::L, Modifiers::NONE) {
            self.send(Action::SetLoop(!view.settings.loop_track));
        }
        if pressed(Key::N, Modifiers::NONE) {
            self.send(Action::Next);
        }
        if pressed(Key::P, Modifiers::NONE) {
            self.send(Action::Prev);
        }
        if pressed(Key::Delete, Modifiers::NONE)
            && let Some(id) = self.selected
        {
            self.remove_row(view, id);
        }
        if pressed(Key::O, Modifiers::NONE) {
            self.dialog.pick_multiple();
        }
        if pressed(Key::R, Modifiers::NONE) {
            self.send(Action::Rerender);
        }
        if pressed(Key::Enter, Modifiers::NONE)
            && let Some(id) = self.selected
        {
            self.send(Action::Play(id));
        }
    }

    /// Removes a row; the selection moves to the row that takes its place.
    fn remove_row(&mut self, view: &View, id: RowId) {
        let rows = &view.playlist.rows;
        if let Some(i) = view.playlist.index_of(id) {
            self.selected = rows.get(i + 1).or_else(|| i.checked_sub(1).and_then(|j| rows.get(j))).map(|r| r.id);
        }
        self.send(Action::Remove(id));
    }

    fn handle_drops(&mut self) {
        let dropped: Vec<PathBuf> =
            self.ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            self.send(Action::Add { paths: dropped, replace: false });
        }
    }

    // ------------------------------------------------------------------ drawing

    fn playlist_panel(&mut self, ui: &mut egui::Ui, view: &View) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Playlist").color(theme::LABEL).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Add\u{2026}").on_hover_text("Add files or folders (O)").clicked() {
                    self.dialog.pick_multiple();
                }
            });
        });
        ui.add_space(2.0);
        let rows = &view.playlist.rows;
        if rows.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Nothing here yet.").color(theme::LABEL));
            return;
        }

        let mut play: Option<RowId> = None;
        let mut clear = false;
        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
            // The footer, pinned to the bottom of the panel: how much is here, and a way out.
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let total: f64 = rows.iter().filter_map(|r| r.duration).sum();
                let count = rows.len();
                let summary =
                    format!("{count} {}  \u{00b7}  {}", if count == 1 { "track" } else { "tracks" }, fmt_clock(total));
                ui.label(RichText::new(summary).color(theme::LABEL));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Clear").on_hover_text("Empty the playlist").clicked() {
                        clear = true;
                    }
                });
            });
            ui.separator();
            ui.with_layout(Layout::top_down(Align::LEFT), |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    let playing = view.playing();
                    for (i, row) in rows.iter().enumerate() {
                        let is_current = view.playlist.current == Some(i);
                        let response = playlist_row(ui, i, row, is_current, self.selected == Some(row.id), playing);
                        if response.clicked() {
                            self.selected = Some(row.id);
                        }
                        if response.double_clicked() {
                            play = Some(row.id);
                        }
                    }
                });
            });
        });
        if clear {
            self.selected = None;
            self.send(Action::Clear);
        }
        if let Some(id) = play {
            self.send(Action::Play(id));
        }
    }

    fn empty_state(&mut self, ui: &mut egui::Ui, view: &View) {
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.28).max(12.0));
            egui::Frame::new()
                .fill(theme::SURFACE)
                .stroke(Stroke::new(1.0, theme::BORDER))
                .corner_radius(10.0)
                .inner_margin(egui::Margin::symmetric(36, 28))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        if view.playlist.rows.is_empty() {
                            ui.label(RichText::new("Drop .syr files or folders here").size(20.0));
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new("A track starts playing as soon as its first block has rendered.")
                                    .color(theme::LABEL),
                            );
                            ui.add_space(16.0);
                            if ui.add(egui::Button::new("Add files\u{2026}").min_size(Vec2::new(140.0, 32.0))).clicked()
                            {
                                self.dialog.pick_multiple();
                            }
                        } else {
                            ui.label(RichText::new("Nothing playing").size(20.0));
                            ui.add_space(6.0);
                            ui.label(RichText::new("Double-click a track, or press Space.").color(theme::LABEL));
                        }
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new("Space play  \u{00b7}  Left / Right seek  \u{00b7}  N / P next and previous")
                                .color(theme::LABEL),
                        );
                    });
                });
        });
    }

    fn header(&mut self, ui: &mut egui::Ui, track: &Track) {
        ui.label(RichText::new(&track.name).heading());
        let layers = track.stem_names.len();
        let summary = format!(
            "{}  \u{00b7}  {} kHz  \u{00b7}  {}  \u{00b7}  {} {}",
            if track.channels == 1 { "mono" } else { "stereo" },
            fmt_khz(track.sample_rate),
            fmt_clock(track.duration),
            layers,
            if layers == 1 { "layer" } else { "layers" }
        );
        ui.label(RichText::new(summary).color(theme::LABEL));
    }

    /// The seek bar: the sound's picture, the part heard brighter than the part to come, the
    /// unrendered tail hatched, a ruler under it, and the time under the pointer. A click seeks;
    /// a drag moves the playhead with the pointer and seeks where it is let go.
    fn overview(&mut self, ui: &mut egui::Ui, view: &View, track: &Arc<Track>) {
        let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 112.0), Sense::click_and_drag());
        let painter = ui.painter_at(rect.expand(1.0));
        painter.rect_filled(rect, 6.0, theme::WAVE_BG);

        let fraction_at = |x: f32| ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        if response.dragged()
            && let Some(pos) = response.interact_pointer_pos()
        {
            self.scrub = Some(fraction_at(pos.x));
        }
        let playhead_x = match self.scrub {
            Some(f) => Some(rect.left() + rect.width() * f),
            None => view.position().map(|p| rect.left() + rect.width() * p as f32 / track.frames.max(1) as f32),
        };

        let overview = track.overview.lock().unwrap().clone();
        let total_columns = track.frames.div_ceil(overview.frames_per_column.max(1)).max(1);
        let column_width = rect.width() / total_columns as f32;
        let mid = rect.center().y;
        let half = rect.height() * 0.5 - 6.0;
        for (c, (lo, hi)) in overview.columns.iter().enumerate().take(overview.ready) {
            let x = rect.left() + (c as f32 + 0.5) * column_width;
            let y0 = mid - hi.clamp(-1.0, 1.0) * half;
            let y1 = mid - lo.clamp(-1.0, 1.0) * half;
            let color = if playhead_x.is_some_and(|p| x <= p) { theme::WAVE_PLAYED } else { theme::WAVE };
            painter.line_segment(
                [egui::pos2(x, y0.min(mid - 0.5)), egui::pos2(x, y1.max(mid + 0.5))],
                Stroke::new(column_width.max(1.0), color),
            );
        }
        // The unrendered tail: hatched over, from the slowest layer's frontier to the end.
        let unity = view.gains.as_ref().is_none_or(|g| g.all_unity());
        let rendered_to = track.rendered_to(unity).unwrap_or(0);
        if rendered_to < track.frames {
            let x = rect.left() + rect.width() * rendered_to as f32 / track.frames as f32;
            let tail = Rect::from_min_max(egui::pos2(x, rect.top()), rect.max);
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
        if let Some(x) = playhead_x {
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                Stroke::new(2.0, theme::PLAYHEAD),
            );
        }
        // The time under the pointer (or being dragged to), and where a click would land.
        if let Some(x) = response.hover_pos().map(|p| p.x).or(self.scrub.map(|f| rect.left() + rect.width() * f)) {
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                Stroke::new(1.0, theme::LABEL.gamma_multiply(0.6)),
            );
            let label = fmt_time(fraction_at(x) as f64 * track.duration);
            let galley = painter.layout_no_wrap(label, egui::FontId::monospace(13.0), theme::TEXT);
            let size = galley.size() + Vec2::new(10.0, 4.0);
            let left = (x - size.x * 0.5).clamp(rect.left() + 2.0, rect.right() - size.x - 2.0);
            let tag = Rect::from_min_size(egui::pos2(left, rect.top() + 4.0), size);
            painter.rect_filled(tag, 4.0, theme::BAR);
            painter.galley(tag.min + Vec2::new(5.0, 2.0), galley, theme::TEXT);
        }
        let seek_to = if response.drag_stopped() {
            self.scrub.take()
        } else if response.clicked() {
            response.interact_pointer_pos().map(|p| fraction_at(p.x))
        } else {
            None
        };
        if let Some(f) = seek_to {
            self.send(Action::Seek((f as f64 * track.frames as f64) as usize));
        }

        // The ruler: ticks at a step that keeps the labels at least 72 px apart.
        let (ruler, _) = ui.allocate_exact_size(Vec2::new(rect.width(), 18.0), Sense::hover());
        let painter = ui.painter_at(ruler);
        let per_second = rect.width() as f64 / track.duration.max(0.001);
        let step = [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0]
            .into_iter()
            .find(|s| s * per_second >= 72.0)
            .unwrap_or(600.0);
        let mut t = 0.0;
        while t <= track.duration + 1e-9 {
            let x = ruler.left() + (t * per_second) as f32;
            painter.line_segment(
                [egui::pos2(x, ruler.top()), egui::pos2(x, ruler.top() + 4.0)],
                Stroke::new(1.0, theme::BORDER),
            );
            let align = if t == 0.0 { egui::Align2::LEFT_TOP } else { egui::Align2::CENTER_TOP };
            if x + 24.0 <= ruler.right() || t == 0.0 {
                painter.text(
                    egui::pos2(x, ruler.top() + 4.0),
                    align,
                    fmt_clock(t),
                    egui::FontId::proportional(12.0),
                    theme::LABEL,
                );
            }
            t += step;
        }
    }

    /// A row per layer across the whole width, in fixed columns so every row lines up: the
    /// name, a fader with the layer's meter in its groove, the fader's level, mute and solo.
    fn layers(&mut self, ui: &mut egui::Ui, view: &View, track: &Arc<Track>, gains: &Arc<Gains>) {
        // The meters follow what is being heard, and fall away when nothing is.
        let now = Instant::now();
        let key = Arc::as_ptr(track) as usize;
        if self.needles_for != Some(key) || self.needles.len() != track.stem_names.len() {
            self.needles = vec![Needle::new(now); track.stem_names.len()];
            self.needles_for = Some(key);
        }
        let dt = now.duration_since(self.needles_at).as_secs_f32().min(0.25);
        self.needles_at = now;
        let peaks = if view.playing() { view.meters.at(view.shared.consumed.load(Ordering::Relaxed)) } else { None };
        for (i, needle) in self.needles.iter_mut().enumerate() {
            needle.update(peaks.as_ref().and_then(|p| p.get(i).copied()), dt, now);
        }

        ui.horizontal(|ui| {
            ui.label(RichText::new("Layers").color(theme::LABEL).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let touched = (0..gains.len()).any(|i| gains.gain(i) != 1.0 || gains.muted(i) || gains.soloed(i));
                if ui
                    .add_enabled(touched, egui::Button::new("Reset"))
                    .on_hover_text("Every fader to unity, no mute, no solo")
                    .clicked()
                {
                    gains.reset();
                }
            });
        });
        ui.add_space(2.0);
        let font = egui::TextStyle::Body.resolve(ui.style());
        let name_width = track
            .stem_names
            .iter()
            .map(|n| ui.fonts_mut(|f| f.layout_no_wrap(n.clone(), font.clone(), theme::TEXT).size().x))
            .fold(0.0_f32, f32::max)
            .clamp(56.0, 180.0);
        const ROW: f32 = 30.0;
        const GAP: f32 = 10.0;
        const LEVEL: f32 = 72.0;
        const CHIP: f32 = 32.0;
        let effective = gains.effective();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, name) in track.stem_names.iter().enumerate() {
                let gain = gains.gain(i);
                let muted = gains.muted(i);
                let soloed = gains.soloed(i);
                let silent = muted || effective[i] == 0.0;
                let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW), Sense::hover());
                // Columns measured from the row's edges, never from what the previous one drew.
                let column =
                    |left: f32, width: f32| Rect::from_min_size(egui::pos2(left, row.top()), Vec2::new(width, ROW));
                let solo = column(row.right() - CHIP, CHIP);
                let mute = column(solo.left() - 6.0 - CHIP, CHIP);
                let level = column(mute.left() - GAP - LEVEL, LEVEL);
                let label = column(row.left(), name_width);
                let fader_left = label.right() + GAP;
                let travel = column(fader_left, (level.left() - GAP - fader_left).max(40.0));
                let text_color = if silent { theme::LABEL } else { theme::TEXT };

                cell(ui, label, Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(name).color(text_color)).truncate()).on_hover_text(name);
                });
                let id = egui::Id::new(("layer-fader", i));
                if let Some(moved) = fader::fader(ui, travel, id, gain, &self.needles[i], silent) {
                    gains.set_gain(i, moved);
                }
                let gain = gains.gain(i);
                cell(ui, level, Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(fmt_db(gain)).monospace().color(text_color));
                });
                cell(ui, mute, Layout::left_to_right(Align::Center), |ui| {
                    if chip(ui, "M", muted, theme::MUTE).on_hover_text("Mute").clicked() {
                        gains.set_mute(i, !muted);
                    }
                });
                cell(ui, solo, Layout::left_to_right(Align::Center), |ui| {
                    if chip(ui, "S", soloed, theme::ACCENT).on_hover_text("Solo").clicked() {
                        gains.set_solo(i, !soloed);
                    }
                });
            }
        });
    }

    /// The transport, in the same place whatever is loaded: previous, play, next, stop, the time,
    /// loop, and the volume.
    fn transport_bar(&mut self, ui: &mut egui::Ui, view: &View) {
        let has_track = view.track.is_some();
        let playing = view.playing();
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let icon = |glyph: &str| RichText::new(glyph).size(16.0);
            let small = Vec2::new(36.0, 32.0);
            if ui
                .add_enabled(view.playlist.prev().is_some(), egui::Button::new(icon("\u{23ee}")).min_size(small))
                .on_hover_text("Previous (P)")
                .clicked()
            {
                self.send(Action::Prev);
            }
            let can_play = has_track || !view.playlist.rows.is_empty();
            let play = egui::Button::new(icon(if playing { "\u{23f8}" } else { "\u{25b6}" }).color(theme::ON_ACCENT))
                .fill(theme::ACCENT)
                .corner_radius(16.0)
                .min_size(Vec2::new(48.0, 32.0));
            if ui.add_enabled(can_play, play).on_hover_text("Play / pause (Space)").clicked() {
                self.send(Action::TogglePlay { fallback: self.selected });
            }
            if ui
                .add_enabled(view.playlist.next().is_some(), egui::Button::new(icon("\u{23ed}")).min_size(small))
                .on_hover_text("Next (N)")
                .clicked()
            {
                self.send(Action::Next);
            }
            if ui
                .add_enabled(has_track, egui::Button::new(icon("\u{23f9}")).min_size(small))
                .on_hover_text("Stop")
                .clicked()
            {
                self.send(Action::Stop);
            }
            ui.add_space(10.0);
            let time = match &view.track {
                Some(track) => {
                    let position = view.position().unwrap_or(0) as f64 / track.sample_rate as f64;
                    format!("{} / {}", fmt_time(position), fmt_time(track.duration))
                }
                None => "-:--.- / -:--.-".into(),
            };
            ui.label(RichText::new(time).monospace().size(15.0));
            ui.add_space(10.0);
            let looping = view.settings.loop_track;
            if chip(ui, "Loop", looping, theme::ACCENT).on_hover_text("Repeat this track (L)").clicked() {
                self.send(Action::SetLoop(!looping));
            }
            // The volume, from the right: the percentage, the slider, and its label when the
            // window is wide enough; a narrow window gets a shorter slider and no label.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let mut volume = view.shared.volume();
                let room = ui.available_width() - 12.0;
                let labelled = room >= 240.0;
                ui.allocate_ui_with_layout(Vec2::new(40.0, 24.0), Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(format!("{:.0}%", volume * 100.0)).monospace());
                });
                ui.spacing_mut().slider_width = if labelled { 120.0 } else { (room - 60.0).clamp(48.0, 120.0) };
                if ui
                    .add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false))
                    .on_hover_text("Volume (Up / Down)")
                    .changed()
                {
                    self.send(Action::SetVolume(volume));
                }
                if labelled {
                    ui.label(RichText::new("Volume").color(theme::LABEL));
                }
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, view: &View) {
        ui.horizontal_centered(|ui| {
            let (text, color) = status_text(view);
            ui.label(RichText::new(text).color(color));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.menu_button("\u{2630}", |ui| {
                    if ui.button("Clear render cache").clicked() {
                        self.send(Action::ClearCache);
                        ui.close();
                    }
                    if ui.button("Re-render this track (R)").clicked() {
                        self.send(Action::Rerender);
                        ui.close();
                    }
                    ui.menu_button("Shortcuts", |ui| {
                        egui::Grid::new("shortcuts").spacing(Vec2::new(18.0, 4.0)).show(ui, |ui| {
                            for (key, what) in SHORTCUTS {
                                ui.label(RichText::new(*key).monospace());
                                ui.label(RichText::new(*what).color(theme::LABEL));
                                ui.end_row();
                            }
                        });
                    });
                    ui.label(RichText::new(format!("cache: {}", self.cache_root.display())).color(theme::LABEL));
                });
                let mut reload = view.settings.reload_on_change;
                if ui
                    .checkbox(&mut reload, "Reload on change")
                    .on_hover_text("Re-render when the source or an import changes")
                    .changed()
                {
                    self.send(Action::SetReloadOnChange(reload));
                }
                self.device_picker(ui, view);
            });
        });
        // A note leaves the status line on time even when nothing else asks for a frame.
        if let Some((_, at)) = &view.note
            && at.elapsed() < NOTE_FOR
        {
            self.ctx.request_repaint_after(NOTE_FOR - at.elapsed());
        }
    }

    fn device_picker(&mut self, ui: &mut egui::Ui, view: &View) {
        let current = view.output.as_ref().map_or("no output device".to_string(), |o| o.device_name.clone());
        let mut pick: Option<Option<String>> = None;
        egui::ComboBox::from_id_salt("device").selected_text(current).width(220.0).show_ui(ui, |ui| {
            // Listing is slow on some systems; the session does it, at most once a second.
            if self.devices_asked.is_none_or(|at| at.elapsed() > Duration::from_secs(1)) {
                self.devices_asked = Some(Instant::now());
                self.send(Action::ListDevices);
            }
            if ui.selectable_label(view.settings.device.is_none(), "default device").clicked() {
                pick = Some(None);
            }
            for d in &view.devices {
                let label = if d.is_default { format!("{} (default)", d.name) } else { d.name.clone() };
                if ui.selectable_label(view.settings.device.as_deref() == Some(d.name.as_str()), label).clicked() {
                    pick = Some(Some(d.name.clone()));
                }
            }
        });
        if let Some(choice) = pick {
            self.send(Action::SetDevice(choice));
        }
    }
}

impl eframe::App for PlayerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // One view per frame, let go when the frame ends.
        let view = self.session.view();
        self.follow(&view);
        self.poll_screenshot();
        self.handle_drops();
        self.handle_keys(&view);

        self.dialog.update(&self.ctx.clone());
        if let Some(paths) = self.dialog.take_picked_multiple() {
            self.send(Action::Add { paths, replace: false });
        }

        let bar =
            |margin_y: i8| egui::Frame::new().fill(theme::BAR).inner_margin(egui::Margin::symmetric(12, margin_y));
        egui::Panel::bottom("status").frame(bar(4)).show(ui, |ui| self.status_bar(ui, &view));
        egui::Panel::bottom("transport").frame(bar(8)).show(ui, |ui| self.transport_bar(ui, &view));
        egui::Panel::left("playlist")
            .resizable(true)
            .default_size(260.0)
            .min_size(180.0)
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::same(10)))
            .show(ui, |ui| self.playlist_panel(ui, &view));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::symmetric(16, 12)))
            .show(ui, |ui| match (&view.track, &view.gains) {
                (Some(track), Some(gains)) => {
                    self.header(ui, track);
                    ui.add_space(10.0);
                    self.overview(ui, &view, track);
                    ui.add_space(10.0);
                    self.layers(ui, &view, track, gains);
                }
                _ => self.empty_state(ui, &view),
            });

        // The playhead and the meters move while playing; a paused track only lets them settle.
        if view.playing() {
            self.ctx.request_repaint_after(Duration::from_millis(33));
        } else if view.track.is_some() {
            self.ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if !self.ephemeral {
            eframe::set_value(storage, SETTINGS_KEY, &self.session.view().settings);
        }
    }
}

fn status_text(view: &View) -> (String, Color32) {
    if let Some((note, at)) = &view.note
        && at.elapsed() < NOTE_FOR
    {
        return (note.clone(), theme::WARN);
    }
    let Some(track) = &view.track else {
        let text = if view.playlist.rows.is_empty() { "" } else { "Stopped" };
        return (text.into(), theme::LABEL);
    };
    if let Some(e) = view.mixer.error.lock().unwrap().clone() {
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
        Render::Done { took, rendered } => {
            parts.push(if !rendered { "from cache".into() } else { format!("rendered in {:.1} s", took.as_secs_f64()) })
        }
        Render::Failed(_) => {}
    }
    if view.mixer.waiting.load(Ordering::Relaxed) || view.shared.starved.load(Ordering::Relaxed) {
        parts.push("waiting for the render".into());
    }
    if view.mixer.master_bypassed.load(Ordering::Relaxed) {
        parts.push("master bypassed while faders are moved".into());
    }
    if let Some(out) = &view.output
        && out.sample_rate != track.sample_rate
    {
        parts.push(format!("resampling to {} Hz", out.sample_rate));
    }
    (parts.join("  \u{00b7}  "), theme::LABEL)
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

/// Lays `add` out inside `rect`: one fixed column of a row.
fn cell<R>(ui: &mut egui::Ui, rect: Rect, layout: Layout, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(layout), add).inner
}

/// One playlist row: the number (or the play state of the loaded track), the name and the
/// duration. The whole row is the hit target; an error colours the name and becomes the tooltip.
fn playlist_row(
    ui: &mut egui::Ui,
    i: usize,
    row: &Row,
    is_current: bool,
    is_selected: bool,
    playing: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::click());
    let painter = ui.painter_at(rect);
    if is_selected {
        painter.rect_filled(rect, 4.0, theme::SURFACE);
    } else if response.hovered() {
        painter.rect_filled(rect, 4.0, theme::SURFACE.gamma_multiply(0.6));
    }
    let font = egui::FontId::proportional(14.0);
    let number = if is_current {
        if playing { "\u{25b6}".to_string() } else { "\u{23f8}".to_string() }
    } else {
        format!("{}", i + 1)
    };
    let number_color = if is_current { theme::ACCENT } else { theme::LABEL };
    painter.text(
        egui::pos2(rect.left() + 26.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        number,
        font.clone(),
        number_color,
    );
    let duration = row.duration.map(fmt_clock).unwrap_or_default();
    let duration_galley = painter.layout_no_wrap(duration, font.clone(), theme::LABEL);
    let duration_left = rect.right() - 6.0 - duration_galley.size().x;
    painter.galley(
        egui::pos2(duration_left, rect.center().y - duration_galley.size().y * 0.5),
        duration_galley,
        theme::LABEL,
    );
    let name_color = if row.error.is_some() {
        theme::ERROR
    } else if is_current {
        theme::ACCENT
    } else {
        theme::TEXT
    };
    let mut job = egui::text::LayoutJob::simple_singleline(row.name.clone(), font, name_color);
    job.wrap = egui::text::TextWrapping::truncate_at_width((duration_left - rect.left() - 44.0).max(10.0));
    let name = ui.fonts_mut(|f| f.layout_job(job));
    painter.galley(egui::pos2(rect.left() + 36.0, rect.center().y - name.size().y * 0.5), name, name_color);
    match &row.error {
        Some(e) => response.on_hover_text(e),
        None => response.on_hover_text(row.path.display().to_string()),
    }
}

/// A small toggle that says whether it is on by its fill, not by its text alone.
fn chip(ui: &mut egui::Ui, text: &str, on: bool, color: Color32) -> egui::Response {
    let button = if on {
        egui::Button::new(RichText::new(text).color(theme::ON_ACCENT).strong()).fill(color)
    } else {
        egui::Button::new(RichText::new(text).color(theme::LABEL))
            .fill(theme::SURFACE)
            .stroke(Stroke::new(1.0, theme::BORDER))
    };
    ui.add(button.min_size(Vec2::new(32.0, 24.0)))
}

/// Minutes and whole seconds, for durations at a glance.
pub fn fmt_clock(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

/// A sample rate in kHz without trailing zeros: 48, 44.1.
fn fmt_khz(rate: u32) -> String {
    let khz = rate as f64 / 1000.0;
    if khz.fract() == 0.0 { format!("{khz:.0}") } else { format!("{khz}") }
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
