//! A layer's fader: a console taper (unity at three quarters of the travel, +6 dB at the top,
//! silence at the bottom), a detent at unity, and the layer's meter in its groove on the same
//! scale, so a peak at 0 dBFS reaches the unity mark.

use std::time::{Duration, Instant};

use egui::{Color32, Rect, Sense, Stroke, StrokeKind, Vec2, pos2};

use crate::theme;

/// Travel against decibels, straight lines between the marks. Below the first mark the gain
/// falls linearly to silence.
const LAW: [(f32, f32); 6] = [(0.05, -60.0), (0.15, -40.0), (0.30, -20.0), (0.50, -10.0), (0.75, 0.0), (1.0, 6.0)];
/// Where unity sits on the travel.
pub const UNITY: f32 = 0.75;
/// A fader within this many dB of unity is exactly unity: the canonical balance, which is what
/// lets a whole-buffer master play its cached mix.
const DETENT_DB: f32 = 0.5;
/// How fast a meter falls, and how long its peak mark holds before falling too.
const FALL_DB_PER_SECOND: f32 = 20.0;
const HOLD: Duration = Duration::from_millis(1500);
/// Below this a meter reads as silence.
const FLOOR_DB: f32 = -80.0;
const HANDLE: Vec2 = Vec2::new(12.0, 22.0);

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn gain_to_db(gain: f32) -> f32 {
    20.0 * gain.log10()
}

/// The gain at a point of the travel.
pub fn gain_at(position: f32) -> f32 {
    let p = position.clamp(0.0, 1.0);
    let (first, floor_db) = LAW[0];
    if p < first {
        return db_to_gain(floor_db) * p / first;
    }
    for pair in LAW.windows(2) {
        let ((a, da), (b, db)) = (pair[0], pair[1]);
        if p <= b {
            return db_to_gain(da + (db - da) * (p - a) / (b - a));
        }
    }
    db_to_gain(LAW[LAW.len() - 1].1)
}

/// Where a gain sits on the travel.
pub fn position_of(gain: f32) -> f32 {
    let (first, floor_db) = LAW[0];
    let floor = db_to_gain(floor_db);
    if gain <= 0.0 {
        return 0.0;
    }
    if gain < floor {
        return first * gain / floor;
    }
    let d = gain_to_db(gain);
    for pair in LAW.windows(2) {
        let ((a, da), (b, db)) = (pair[0], pair[1]);
        if d <= db {
            return a + (b - a) * (d - da) / (db - da);
        }
    }
    1.0
}

/// Exactly unity when within the detent of it.
pub fn detent(gain: f32) -> f32 {
    if gain > 0.0 && gain_to_db(gain).abs() < DETENT_DB { 1.0 } else { gain }
}

/// A meter's needle: jumps up to a new peak, falls at a steady rate, and marks the highest
/// recent peak for a moment before that falls too.
#[derive(Clone, Copy, Debug)]
pub struct Needle {
    pub level_db: f32,
    pub hold_db: f32,
    held_at: Instant,
}

impl Needle {
    pub fn new(now: Instant) -> Needle {
        Needle { level_db: f32::NEG_INFINITY, hold_db: f32::NEG_INFINITY, held_at: now }
    }

    /// Moves to the latest peak (`None` while nothing plays), `dt` seconds after the last move.
    pub fn update(&mut self, peak: Option<f32>, dt: f32, now: Instant) {
        let target = match peak {
            Some(p) if p > 0.0 && gain_to_db(p) > FLOOR_DB => gain_to_db(p),
            _ => f32::NEG_INFINITY,
        };
        let fall = FALL_DB_PER_SECOND * dt;
        self.level_db = target.max(self.level_db - fall);
        if self.level_db < FLOOR_DB {
            self.level_db = f32::NEG_INFINITY;
        }
        if target >= self.hold_db {
            self.hold_db = target;
            self.held_at = now;
        } else if now.duration_since(self.held_at) > HOLD {
            self.hold_db = self.level_db.max(self.hold_db - fall);
            if self.hold_db < FLOOR_DB {
                self.hold_db = f32::NEG_INFINITY;
            }
        }
    }
}

/// The meter's colour at a level: clear below -6 dBFS, amber to 0, red over.
fn zone_color(db: f32) -> Color32 {
    if db > 0.0 {
        theme::ERROR
    } else if db > -6.0 {
        theme::WARN
    } else {
        theme::METER
    }
}

/// Draws a fader with its meter in `rect`; returns the new gain when it was moved. A drag moves
/// it by the pointer's travel from wherever it is grabbed (Shift for a tenth of that), so a
/// stray click never throws a layer to a new level; a double-click returns it to unity.
pub fn fader(ui: &mut egui::Ui, rect: Rect, id: egui::Id, gain: f32, needle: &Needle, silent: bool) -> Option<f32> {
    let response = ui
        .interact(rect, id, Sense::click_and_drag())
        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
        .on_hover_text("Drag to set, Shift for fine; double-click for unity");
    // The handle's centre runs from one end of the travel to the other.
    let travel = rect.shrink2(Vec2::new(HANDLE.x * 0.5, 0.0));
    let x_of = |p: f32| travel.left() + travel.width() * p;

    let mut moved = None;
    if response.double_clicked() {
        moved = Some(1.0);
    } else if response.dragged() {
        // The position being dragged, kept apart from the gain so the detent cannot hold the
        // fader: it snaps the gain, not the hand.
        let start = if response.drag_started() {
            position_of(gain)
        } else {
            ui.data(|d| d.get_temp(id)).unwrap_or(position_of(gain))
        };
        let scale = if ui.input(|i| i.modifiers.shift) { 0.1 } else { 1.0 };
        let position = (start + response.drag_delta().x * scale / travel.width().max(1.0)).clamp(0.0, 1.0);
        ui.data_mut(|d| d.insert_temp(id, position));
        let target = detent(gain_at(position));
        if target != gain {
            moved = Some(target);
        }
    }
    let gain = moved.unwrap_or(gain);

    let painter = ui.painter_at(rect.expand(2.0));
    let groove =
        Rect::from_min_max(pos2(travel.left(), rect.center().y - 5.0), pos2(travel.right(), rect.center().y + 5.0));
    painter.rect_filled(groove, 4.0, theme::WAVE_BG);
    painter.rect_stroke(groove, 4.0, Stroke::new(1.0, theme::BORDER), StrokeKind::Inside);

    // The meter, in three zones along the same scale as the fader.
    let inner = groove.shrink(2.0);
    let inner_x = |p: f32| inner.left() + inner.width() * p;
    if needle.level_db.is_finite() {
        let level = position_of(db_to_gain(needle.level_db));
        let zones = [(position_of(db_to_gain(-6.0)), theme::METER), (UNITY, theme::WARN), (1.0, theme::ERROR)];
        let mut from = 0.0;
        for (to, color) in zones {
            let end = level.min(to);
            if end > from {
                painter.rect_filled(
                    Rect::from_min_max(pos2(inner_x(from), inner.top()), pos2(inner_x(end), inner.bottom())),
                    1.0,
                    color,
                );
            }
            if level <= to {
                break;
            }
            from = to;
        }
    }
    if needle.hold_db.is_finite() {
        let x = inner_x(position_of(db_to_gain(needle.hold_db)));
        painter.line_segment(
            [pos2(x, inner.top()), pos2(x, inner.bottom())],
            Stroke::new(2.0, zone_color(needle.hold_db)),
        );
    }

    // Unity, marked above and below the groove.
    let ux = x_of(UNITY);
    let mark = Stroke::new(1.0, theme::LABEL);
    painter.line_segment([pos2(ux, groove.top() - 5.0), pos2(ux, groove.top() - 1.0)], mark);
    painter.line_segment([pos2(ux, groove.bottom() + 1.0), pos2(ux, groove.bottom() + 5.0)], mark);

    let hx = x_of(position_of(gain));
    let handle = Rect::from_center_size(pos2(hx, rect.center().y), HANDLE);
    let fill = if response.dragged() || response.hovered() {
        theme::TEXT
    } else if silent {
        theme::BORDER.lerp_to_gamma(theme::LABEL, 0.5)
    } else {
        theme::LABEL
    };
    painter.rect_filled(handle, 3.0, fill);
    painter.line_segment([pos2(hx, handle.top() + 5.0), pos2(hx, handle.bottom() - 5.0)], Stroke::new(1.5, theme::BG));
    moved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_law_puts_unity_at_three_quarters_and_runs_one_way() {
        assert_eq!(gain_at(UNITY), 1.0, "unity is exact at its mark");
        assert_eq!(position_of(1.0), UNITY);
        assert_eq!(gain_at(0.0), 0.0, "the bottom is silence");
        assert_eq!(position_of(0.0), 0.0);
        assert!((gain_to_db(gain_at(1.0)) - 6.0).abs() < 1e-4, "the top is +6 dB");
        let mut last = -1.0;
        for step in 0..=1000 {
            let p = step as f32 / 1000.0;
            let g = gain_at(p);
            assert!(g > last || (p == 0.0 && g == 0.0), "the gain rises all the way up: {p}");
            last = g;
            assert!((position_of(g) - p).abs() < 1e-4, "position and gain are inverses at {p}");
        }
    }

    #[test]
    fn the_detent_catches_near_unity_only() {
        assert_eq!(detent(db_to_gain(0.3)), 1.0);
        assert_eq!(detent(db_to_gain(-0.3)), 1.0);
        let off = db_to_gain(-0.8);
        assert_eq!(detent(off), off);
        assert_eq!(detent(0.0), 0.0);
    }

    #[test]
    fn a_needle_jumps_up_falls_steadily_and_holds_its_peak() {
        let t0 = Instant::now();
        let mut needle = Needle::new(t0);
        needle.update(Some(1.0), 0.05, t0);
        assert_eq!(needle.level_db, 0.0, "a new peak shows at once");
        let t1 = t0 + Duration::from_millis(500);
        needle.update(None, 0.5, t1);
        assert!((needle.level_db + 10.0).abs() < 1e-3, "half a second falls 10 dB");
        assert_eq!(needle.hold_db, 0.0, "the peak mark holds");
        let t2 = t0 + Duration::from_secs(2);
        needle.update(None, 1.5, t2);
        assert!(needle.hold_db < 0.0, "and falls once the hold is over");
        needle.update(None, 10.0, t2 + Duration::from_secs(10));
        assert!(!needle.level_db.is_finite() && !needle.hold_db.is_finite(), "silence reads as nothing");
    }
}
