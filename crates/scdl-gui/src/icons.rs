//! Vector icons.
//!
//! Drawn as line segments rather than glyphs on purpose: egui's bundled fonts
//! cover only a subset of the symbol range, and characters like `⌕`, `✓` and
//! `☾` render as tofu boxes. Painting them means the UI looks the same on every
//! machine regardless of installed fonts, and the icons scale cleanly.

use egui::{Color32, Painter, Pos2, Stroke, Vec2};

/// Stroke width that keeps icons looking consistent across sizes.
fn weight(size: f32) -> f32 {
    (size * 0.11).clamp(1.2, 2.4)
}

/// Magnifying glass.
pub fn search(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let s = Stroke::new(weight(size), color);
    let r = size * 0.32;
    let c = center - Vec2::splat(size * 0.08);
    painter.circle_stroke(c, r, s);
    let start = c + Vec2::angled(std::f32::consts::FRAC_PI_4) * r;
    painter.line_segment([start, start + Vec2::splat(size * 0.26)], s);
}

/// Downward arrow into a tray.
pub fn download(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let s = Stroke::new(weight(size), color);
    let h = size * 0.34;
    let top = center - Vec2::new(0.0, h);
    let bottom = center + Vec2::new(0.0, h * 0.35);
    painter.line_segment([top, bottom], s);
    // Chevron.
    let w = size * 0.22;
    painter.line_segment([bottom, bottom + Vec2::new(-w, -w)], s);
    painter.line_segment([bottom, bottom + Vec2::new(w, -w)], s);
    // Tray.
    let y = center.y + h * 0.75;
    let hw = size * 0.34;
    painter.line_segment(
        [Pos2::new(center.x - hw, y), Pos2::new(center.x + hw, y)],
        s,
    );
}

/// Music note.
pub fn note(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let s = Stroke::new(weight(size), color);
    let stem_x = center.x + size * 0.16;
    let top = Pos2::new(stem_x, center.y - size * 0.36);
    let bottom = Pos2::new(stem_x, center.y + size * 0.22);
    painter.line_segment([top, bottom], s);
    // Flag.
    painter.line_segment([top, top + Vec2::new(size * 0.22, size * 0.14)], s);
    // Note head.
    painter.circle_filled(
        bottom - Vec2::new(size * 0.15, -size * 0.04),
        size * 0.16,
        color,
    );
}

/// Gear, drawn as a ring with radial teeth.
pub fn gear(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let s = Stroke::new(weight(size), color);
    let r = size * 0.24;
    painter.circle_stroke(center, r, s);
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::TAU / 8.0;
        let dir = Vec2::angled(a);
        painter.line_segment(
            [center + dir * (r + 1.5), center + dir * (r + size * 0.16)],
            s,
        );
    }
}

/// Tick.
pub fn check(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let s = Stroke::new(weight(size) * 1.1, color);
    let a = center + Vec2::new(-size * 0.28, 0.0);
    let b = center + Vec2::new(-size * 0.06, size * 0.22);
    let c = center + Vec2::new(size * 0.30, -size * 0.24);
    painter.line_segment([a, b], s);
    painter.line_segment([b, c], s);
}

/// Cross.
pub fn cross(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let s = Stroke::new(weight(size) * 1.1, color);
    let d = size * 0.24;
    painter.line_segment([center + Vec2::new(-d, -d), center + Vec2::new(d, d)], s);
    painter.line_segment([center + Vec2::new(d, -d), center + Vec2::new(-d, d)], s);
}

/// Horizontal dash, for "skipped".
pub fn dash(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let s = Stroke::new(weight(size) * 1.1, color);
    let d = size * 0.26;
    painter.line_segment([center - Vec2::new(d, 0.0), center + Vec2::new(d, 0.0)], s);
}

/// Small filled dot, for "queued".
pub fn dot(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    painter.circle_filled(center, size * 0.12, color);
}

/// Crescent moon: a filled disc with an offset disc punched out using the
/// background colour, which is cheaper than a real boolean path.
pub fn moon(painter: &Painter, center: Pos2, size: f32, color: Color32, bg: Color32) {
    let r = size * 0.32;
    painter.circle_filled(center, r, color);
    painter.circle_filled(center + Vec2::new(size * 0.15, -size * 0.09), r * 0.92, bg);
}

/// Sun.
pub fn sun(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let s = Stroke::new(weight(size), color);
    let r = size * 0.19;
    painter.circle_filled(center, r, color);
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::TAU / 8.0;
        let dir = Vec2::angled(a);
        painter.line_segment(
            [center + dir * (r + 2.5), center + dir * (r + size * 0.20)],
            s,
        );
    }
}

/// Equalizer bars, used as the wordmark.
pub fn equalizer(
    painter: &Painter,
    rect: egui::Rect,
    time: f32,
    active: bool,
    color_at: impl Fn(f32) -> Color32,
) {
    let bars = 4;
    let w = 4.0;
    let gap = 3.0;
    let total = bars as f32 * w + (bars - 1) as f32 * gap;
    let x0 = rect.center().x - total * 0.5;

    for i in 0..bars {
        let phase = i as f32 * 1.7;
        let h = if active {
            (0.35 + 0.65 * ((time * 6.0 + phase).sin() * 0.5 + 0.5)) * rect.height()
        } else {
            rect.height() * (0.3 + 0.12 * i as f32)
        };
        let x = x0 + i as f32 * (w + gap);
        painter.rect_filled(
            egui::Rect::from_min_size(Pos2::new(x, rect.bottom() - h), Vec2::new(w, h)),
            egui::CornerRadius::same(2),
            color_at(i as f32 / bars as f32),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stroke_weight_is_bounded() {
        // Icons are drawn from 10px (inline status) to ~40px (empty states);
        // the stroke must stay visible without turning into a blob.
        for size in [4.0f32, 10.0, 18.0, 40.0, 200.0] {
            let w = weight(size);
            assert!((1.2..=2.4).contains(&w), "size {size} -> weight {w}");
        }
    }
}
