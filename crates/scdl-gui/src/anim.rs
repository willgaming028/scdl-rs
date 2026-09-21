//! Easing curves, spring physics, and the custom-painted animated widgets.
//!
//! egui is immediate-mode: every frame recomputes the whole UI, so an animation
//! is just "compute the value for this instant and draw it". Two sources of
//! animation state are used here:
//!
//! * `Context::animate_*` for anything keyed to a bool or a target value — egui
//!   stores the in-flight value against an `Id` for us.
//! * Explicit structs ([`Spring`], [`Particles`], [`Spectrum`]) where the motion
//!   needs its own physics or a per-element history that egui cannot hold.
//!
//! Everything here is deterministic given `(time, dt, inputs)`, which is what
//! makes the screenshot harness reproducible.

use std::f32::consts::TAU;

use egui::{
    epaint::{Mesh, PathShape, Vertex},
    Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2,
};

use crate::theme::{lerp_color, Palette};

// ---------------------------------------------------------------- easing ---

pub mod ease {
    /// Decelerating; the default for things entering the screen.
    pub fn out_cubic(t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        1.0 - (1.0 - t).powi(3)
    }

    /// Overshoots slightly then settles — good for things that "pop" in.
    pub fn out_back(t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        const C1: f32 = 1.70158;
        const C3: f32 = C1 + 1.0;
        1.0 + C3 * (t - 1.0).powi(3) + C1 * (t - 1.0).powi(2)
    }

    /// Springy overshoot with a few oscillations.
    pub fn out_elastic(t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        if t == 0.0 || t == 1.0 {
            return t;
        }
        let c4 = std::f32::consts::TAU / 3.0;
        2f32.powf(-10.0 * t) * ((t * 10.0 - 0.75) * c4).sin() + 1.0
    }

    pub fn out_quint(t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        1.0 - (1.0 - t).powi(5)
    }

    /// Smooth 0..1 ramp with zero derivative at both ends.
    pub fn smoothstep(t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }
}

// --------------------------------------------------------------- springs ---

/// A critically-damped-ish spring, integrated per frame.
///
/// Preferred over a fixed-duration tween when the target keeps moving (download
/// progress, a value that updates while the previous animation is still
/// running), because it has no notion of "start" to restart from.
#[derive(Debug, Clone, Copy)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub stiffness: f32,
    pub damping: f32,
}

impl Spring {
    pub fn new(value: f32) -> Self {
        Self {
            value,
            velocity: 0.0,
            stiffness: 120.0,
            damping: 18.0,
        }
    }

    pub fn snappy(value: f32) -> Self {
        Self {
            stiffness: 220.0,
            damping: 26.0,
            ..Self::new(value)
        }
    }

    /// Advance toward `target`. `dt` is clamped so a stalled frame cannot make
    /// the integrator explode.
    pub fn step(&mut self, target: f32, dt: f32) -> f32 {
        let dt = dt.clamp(0.0, 1.0 / 20.0);
        let accel = self.stiffness * (target - self.value) - self.damping * self.velocity;
        self.velocity += accel * dt;
        self.value += self.velocity * dt;
        if (target - self.value).abs() < 1e-4 && self.velocity.abs() < 1e-3 {
            self.value = target;
            self.velocity = 0.0;
        }
        self.value
    }
}

// ------------------------------------------------------------ primitives ---

/// Sample points along an arc.
fn arc_points(center: Pos2, radius: f32, start: f32, end: f32, segments: usize) -> Vec<Pos2> {
    let segments = segments.max(2);
    (0..=segments)
        .map(|i| {
            let t = i as f32 / segments as f32;
            let a = start + (end - start) * t;
            Pos2::new(center.x + radius * a.cos(), center.y + radius * a.sin())
        })
        .collect()
}

/// A circular progress ring with a soft glow and a centre label.
///
/// `progress` is 0..1. Pass `None` for an indeterminate spinner instead.
pub struct Ring<'a> {
    pub center: Pos2,
    pub radius: f32,
    pub thickness: f32,
    /// `None` draws an indeterminate spinner instead of an arc.
    pub progress: Option<f32>,
    pub label: Option<&'a str>,
}

pub fn progress_ring(painter: &Painter, time: f32, ring: Ring<'_>, p: &Palette) {
    let Ring {
        center,
        radius,
        thickness,
        progress,
        label,
    } = ring;
    // Track.
    painter.circle_stroke(
        center,
        radius,
        Stroke::new(thickness, p.outline.gamma_multiply(0.8)),
    );

    let (start, end) = match progress {
        Some(v) => {
            let v = v.clamp(0.0, 1.0);
            // Start at 12 o'clock.
            let s = -TAU / 4.0;
            (s, s + TAU * v)
        }
        None => {
            // Indeterminate: a comet whose length breathes as it spins.
            let head = time * 2.2;
            let len = TAU * (0.18 + 0.12 * (time * 1.7).sin().abs());
            (head - len, head)
        }
    };

    if (end - start).abs() > 1e-3 {
        let segments = (((end - start).abs() / TAU) * 96.0).ceil().max(4.0) as usize;
        let pts = arc_points(center, radius, start, end, segments);

        // Glow: the same arc drawn fatter and faint, underneath.
        painter.add(Shape::Path(PathShape::line(
            pts.clone(),
            Stroke::new(thickness * 2.6, p.accent.gamma_multiply(0.16)),
        )));

        // The arc itself, drawn as short gradient segments so it shifts hue
        // along its length. A single Stroke cannot do a gradient.
        for w in pts.windows(2).enumerate() {
            let (i, seg) = w;
            let t = i as f32 / (pts.len().max(2) - 1) as f32;
            painter.line_segment(
                [seg[0], seg[1]],
                Stroke::new(thickness, lerp_color(p.accent, p.accent_hi, t)),
            );
        }

        // A bright cap on the leading end.
        if let Some(tip) = pts.last() {
            painter.circle_filled(*tip, thickness * 0.62, p.accent_hi);
            painter.circle_filled(*tip, thickness * 1.5, p.accent_hi.gamma_multiply(0.22));
        }
    }

    if let Some(text) = label {
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(radius * 0.46),
            p.text,
        );
    }
}

/// A small spinning arc, for inline "working" indicators.
pub fn spinner(painter: &Painter, time: f32, center: Pos2, radius: f32, p: &Palette) {
    progress_ring(
        painter,
        time,
        Ring {
            center,
            radius,
            thickness: radius * 0.22,
            progress: None,
            label: None,
        },
        p,
    );
}

/// An animated multi-stop gradient painted as a mesh.
///
/// egui has no gradient primitive, so this builds a grid mesh with per-vertex
/// colours and lets the renderer interpolate between them.
pub fn aurora(painter: &Painter, time: f32, rect: Rect, p: &Palette, intensity: f32) {
    if intensity <= 0.001 || rect.width() < 1.0 || rect.height() < 1.0 {
        return;
    }

    const COLS: usize = 14;
    const ROWS: usize = 10;

    let mut mesh = Mesh::default();

    for row in 0..=ROWS {
        for col in 0..=COLS {
            let u = col as f32 / COLS as f32;
            let v = row as f32 / ROWS as f32;
            let pos = Pos2::new(
                rect.left() + rect.width() * u,
                rect.top() + rect.height() * v,
            );

            // Three drifting sine lobes summed into a scalar field.
            let f = (u * 3.1 + time * 0.23).sin() * 0.5
                + (v * 2.7 - time * 0.17).sin() * 0.35
                + ((u + v) * 2.2 + time * 0.31).sin() * 0.3;
            let t = (f * 0.5 + 0.5).clamp(0.0, 1.0);

            // Strongest at the top-left, fading out downward, so content stays
            // readable over it.
            let falloff = (1.0 - v).powf(1.6) * (1.0 - u * 0.35);
            let alpha = (t * falloff * intensity * 0.5).clamp(0.0, 1.0);

            let color = lerp_color(p.accent, p.accent_alt, t).gamma_multiply(alpha);
            mesh.vertices.push(Vertex {
                pos,
                uv: egui::epaint::WHITE_UV,
                color,
            });
        }
    }

    let idx = |r: usize, c: usize| (r * (COLS + 1) + c) as u32;
    for row in 0..ROWS {
        for col in 0..COLS {
            mesh.indices.extend_from_slice(&[
                idx(row, col),
                idx(row, col + 1),
                idx(row + 1, col),
                idx(row + 1, col),
                idx(row, col + 1),
                idx(row + 1, col + 1),
            ]);
        }
    }

    painter.add(Shape::mesh(mesh));
}

// ------------------------------------------------------------- particles ---

#[derive(Debug, Clone, Copy)]
struct Particle {
    pos: Vec2,
    vel: Vec2,
    radius: f32,
    /// 0..1; drives alpha and parallax so the field reads as having depth.
    depth: f32,
}

/// A drifting particle field, used behind the hero panel.
#[derive(Debug, Clone)]
pub struct Particles {
    items: Vec<Particle>,
    seeded: bool,
}

impl Default for Particles {
    fn default() -> Self {
        Self::new(90)
    }
}

impl Particles {
    pub fn new(count: usize) -> Self {
        Self {
            items: Vec::with_capacity(count),
            seeded: false,
        }
    }

    /// Deterministic pseudo-random, so screenshots are reproducible and no
    /// `rand` dependency is needed.
    fn hash01(n: u32) -> f32 {
        let mut x = n.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
        x ^= x >> 17;
        x = x.wrapping_mul(830_770_091);
        x ^= x >> 11;
        x = x.wrapping_mul(278_297_099);
        x ^= x >> 15;
        (x % 100_000) as f32 / 100_000.0
    }

    fn seed(&mut self, count: usize) {
        self.items.clear();
        for i in 0..count {
            let i = i as u32;
            let depth = 0.25 + Self::hash01(i * 7 + 3) * 0.75;
            self.items.push(Particle {
                pos: Vec2::new(Self::hash01(i * 3 + 1), Self::hash01(i * 5 + 2)),
                vel: Vec2::new(
                    (Self::hash01(i * 11 + 5) - 0.5) * 0.02,
                    -0.012 - Self::hash01(i * 13 + 7) * 0.03,
                ),
                radius: 0.8 + Self::hash01(i * 17 + 11) * 2.2,
                depth,
            });
        }
        self.seeded = true;
    }

    /// Advance and draw. Positions are normalised 0..1 so a resize does not
    /// teleport anything.
    pub fn update_and_draw(
        &mut self,
        painter: &Painter,
        dt: f32,
        rect: Rect,
        p: &Palette,
        intensity: f32,
    ) {
        if !self.seeded {
            let n = self.items.capacity().max(90);
            self.seed(n);
        }
        if intensity <= 0.001 {
            return;
        }
        let dt = dt.clamp(0.0, 1.0 / 20.0);

        for pt in &mut self.items {
            pt.pos += pt.vel * dt * pt.depth;
            // Wrap.
            if pt.pos.y < -0.05 {
                pt.pos.y += 1.1;
            }
            if pt.pos.x < -0.05 {
                pt.pos.x += 1.1;
            } else if pt.pos.x > 1.05 {
                pt.pos.x -= 1.1;
            }

            let screen = Pos2::new(
                rect.left() + pt.pos.x * rect.width(),
                rect.top() + pt.pos.y * rect.height(),
            );
            let alpha = pt.depth * 0.5 * intensity;
            painter.circle_filled(
                screen,
                pt.radius * pt.depth,
                lerp_color(p.accent_hi, p.text_faint, 1.0 - pt.depth).gamma_multiply(alpha),
            );
        }
    }
}

// -------------------------------------------------------------- spectrum ---

/// Bar-graph spectrum with spring smoothing and decaying peak markers.
///
/// Fed either real magnitudes or, when nothing is playing, a synthesised idle
/// pattern so the panel is never a dead rectangle.
#[derive(Debug, Clone)]
pub struct Spectrum {
    bars: Vec<Spring>,
    peaks: Vec<f32>,
}

impl Spectrum {
    pub fn new(n: usize) -> Self {
        Self {
            bars: vec![Spring::snappy(0.0); n],
            peaks: vec![0.0; n],
        }
    }

    pub fn len(&self) -> usize {
        self.bars.len()
    }

    /// A plausible idle animation: layered sines with a low-frequency tilt.
    pub fn idle_magnitudes(&self, time: f32, energy: f32) -> Vec<f32> {
        (0..self.bars.len())
            .map(|i| {
                let x = i as f32 / self.bars.len().max(1) as f32;
                let a = (time * 2.3 + x * 9.0).sin() * 0.5 + 0.5;
                let b = (time * 1.1 + x * 3.0).sin() * 0.5 + 0.5;
                let c = (time * 3.7 - x * 14.0).sin() * 0.5 + 0.5;
                // Tilt down toward the high end, like real music.
                let tilt = (1.0 - x).powf(0.7);
                ((a * 0.5 + b * 0.3 + c * 0.2) * tilt * energy).clamp(0.0, 1.0)
            })
            .collect()
    }

    pub fn update_and_draw(
        &mut self,
        painter: &Painter,
        dt: f32,
        rect: Rect,
        mags: &[f32],
        p: &Palette,
    ) {
        if self.bars.is_empty() || rect.width() < 2.0 {
            return;
        }
        let dt = dt.clamp(0.0, 1.0 / 20.0);

        let n = self.bars.len();
        let gap = 2.0;
        let bar_w = ((rect.width() - gap * (n as f32 - 1.0)) / n as f32).max(1.0);

        for i in 0..n {
            let target = mags.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let v = self.bars[i].step(target, dt);

            // Peak marker falls slowly, jumps instantly.
            if v > self.peaks[i] {
                self.peaks[i] = v;
            } else {
                self.peaks[i] = (self.peaks[i] - dt * 0.55).max(0.0);
            }

            let x = rect.left() + i as f32 * (bar_w + gap);
            let h = (v * rect.height()).max(1.5);
            let bar = Rect::from_min_size(Pos2::new(x, rect.bottom() - h), Vec2::new(bar_w, h));

            let t = i as f32 / n as f32;
            painter.rect_filled(
                bar,
                egui::CornerRadius::same(2),
                lerp_color(p.accent, p.accent_alt, t).gamma_multiply(0.55 + 0.45 * v),
            );

            if self.peaks[i] > 0.02 {
                let py = rect.bottom() - self.peaks[i] * rect.height();
                painter.rect_filled(
                    Rect::from_min_size(Pos2::new(x, py), Vec2::new(bar_w, 1.5)),
                    egui::CornerRadius::ZERO,
                    p.text_dim.gamma_multiply(0.7),
                );
            }
        }
    }
}

// ----------------------------------------------------------------- misc ----

/// Draw a rounded rect with a soft outer glow.
pub fn glow_rect(
    painter: &Painter,
    rect: Rect,
    radius: egui::CornerRadius,
    color: Color32,
    strength: f32,
) {
    if strength <= 0.001 {
        return;
    }
    for i in 1..=3 {
        let grow = i as f32 * 3.0;
        painter.rect_stroke(
            rect.expand(grow),
            radius,
            Stroke::new(2.0, color.gamma_multiply(strength * 0.10 / i as f32)),
            StrokeKind::Outside,
        );
    }
}

/// A shimmer sweep for skeleton placeholders.
pub fn shimmer(painter: &Painter, time: f32, rect: Rect, p: &Palette) {
    painter.rect_filled(rect, crate::theme::radius::SM, p.surface_hi);

    let sweep = ((time * 0.9) % 1.6) / 1.6;
    let band_w = rect.width() * 0.28;
    let x = rect.left() - band_w + sweep * (rect.width() + band_w * 2.0);

    let mut mesh = Mesh::default();
    let cols = 10;
    for i in 0..=cols {
        let t = i as f32 / cols as f32;
        let px = x + band_w * t;
        // Triangular falloff across the band.
        let a = (1.0 - (t - 0.5).abs() * 2.0).clamp(0.0, 1.0) * 0.16;
        let color = p.text.gamma_multiply(a);
        mesh.vertices.push(Vertex {
            pos: Pos2::new(px, rect.top()),
            uv: egui::epaint::WHITE_UV,
            color,
        });
        mesh.vertices.push(Vertex {
            pos: Pos2::new(px, rect.bottom()),
            uv: egui::epaint::WHITE_UV,
            color,
        });
    }
    for i in 0..cols {
        let b = (i * 2) as u32;
        mesh.indices
            .extend_from_slice(&[b, b + 1, b + 2, b + 2, b + 1, b + 3]);
    }

    painter.clone().with_clip_rect(rect).add(Shape::mesh(mesh));
}

/// Format a byte count for display.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub fn human_duration(secs: f32) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "—".into();
    }
    let s = secs as u64;
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}:{:02}", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easings_hit_their_endpoints() {
        for f in [
            ease::out_cubic as fn(f32) -> f32,
            ease::out_quint,
            ease::smoothstep,
        ] {
            assert!((f(0.0) - 0.0).abs() < 1e-5);
            assert!((f(1.0) - 1.0).abs() < 1e-5);
        }
        // These two overshoot on purpose, so only the endpoints are pinned.
        assert!((ease::out_back(0.0)).abs() < 1e-5);
        assert!((ease::out_back(1.0) - 1.0).abs() < 1e-4);
        assert!((ease::out_elastic(0.0)).abs() < 1e-5);
        assert!((ease::out_elastic(1.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn easings_are_clamped_outside_zero_to_one() {
        assert_eq!(ease::out_cubic(-5.0), 0.0);
        assert_eq!(ease::out_cubic(5.0), 1.0);
        assert_eq!(ease::smoothstep(9.0), 1.0);
    }

    #[test]
    fn out_back_actually_overshoots() {
        let peak = (0..100)
            .map(|i| ease::out_back(i as f32 / 99.0))
            .fold(0.0f32, f32::max);
        assert!(peak > 1.0, "out_back should overshoot, peaked at {peak}");
    }

    #[test]
    fn spring_converges_to_its_target() {
        let mut s = Spring::new(0.0);
        for _ in 0..600 {
            s.step(1.0, 1.0 / 60.0);
        }
        assert!((s.value - 1.0).abs() < 1e-3, "settled at {}", s.value);
        assert!(s.velocity.abs() < 1e-2);
    }

    #[test]
    fn spring_survives_a_huge_dt() {
        // A stalled frame must not send the integrator to infinity.
        let mut s = Spring::new(0.0);
        s.step(1.0, 10.0);
        assert!(s.value.is_finite() && s.value.abs() < 100.0, "{}", s.value);
    }

    #[test]
    fn spring_tracks_a_moving_target() {
        let mut s = Spring::new(0.0);
        for i in 0..400 {
            s.step(i as f32 / 400.0, 1.0 / 60.0);
        }
        assert!((s.value - 1.0).abs() < 0.1, "lagged at {}", s.value);
    }

    #[test]
    fn particle_hash_is_deterministic_and_in_range() {
        for i in 0..1000u32 {
            let v = Particles::hash01(i);
            assert!((0.0..1.0).contains(&v), "hash01({i}) = {v}");
            assert_eq!(v, Particles::hash01(i), "not deterministic");
        }
    }

    #[test]
    fn particle_hash_is_reasonably_spread() {
        // A constant hash would make every particle sit on top of the others.
        let mut buckets = [0usize; 10];
        for i in 0..1000u32 {
            buckets[(Particles::hash01(i) * 10.0) as usize % 10] += 1;
        }
        assert!(
            buckets.iter().all(|&b| b > 20),
            "poor distribution: {buckets:?}"
        );
    }

    #[test]
    fn spectrum_idle_magnitudes_stay_in_range() {
        let s = Spectrum::new(24);
        for step in 0..200 {
            let t = step as f32 * 0.05;
            for m in s.idle_magnitudes(t, 1.0) {
                assert!((0.0..=1.0).contains(&m), "magnitude {m} out of range");
            }
        }
    }

    #[test]
    fn spectrum_idle_respects_energy() {
        let s = Spectrum::new(16);
        let quiet = s.idle_magnitudes(1.0, 0.0);
        assert!(quiet.iter().all(|m| *m == 0.0));
    }

    #[test]
    fn arc_points_span_the_requested_angle() {
        let c = Pos2::new(0.0, 0.0);
        let pts = arc_points(c, 10.0, 0.0, std::f32::consts::PI, 32);
        assert_eq!(pts.len(), 33);
        // Starts at (10, 0), ends at (-10, 0).
        assert!((pts[0].x - 10.0).abs() < 1e-3);
        assert!((pts.last().unwrap().x + 10.0).abs() < 1e-3);
    }

    #[test]
    fn human_bytes_reads_sensibly() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn human_duration_handles_nonsense() {
        assert_eq!(human_duration(f32::INFINITY), "—");
        assert_eq!(human_duration(-1.0), "—");
        assert_eq!(human_duration(45.0), "45s");
        assert_eq!(human_duration(125.0), "2:05");
        assert_eq!(human_duration(3725.0), "1h02m");
    }
}
