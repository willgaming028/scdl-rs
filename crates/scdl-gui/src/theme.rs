//! Visual design tokens and the egui style built from them.
//!
//! Everything visual in the app pulls from here so the palette can be changed in
//! one place. Colours are defined in sRGB and converted by egui; the accent is
//! SoundCloud's orange, warmed slightly so it survives being drawn at low alpha.

use egui::{Color32, Margin, Stroke, Style, Visuals};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
}

impl Mode {
    pub fn toggled(self) -> Self {
        match self {
            Mode::Dark => Mode::Light,
            Mode::Light => Mode::Dark,
        }
    }
}

/// The full colour set for one mode.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Window background, furthest back.
    pub bg: Color32,
    /// Raised surfaces: cards, panels.
    pub surface: Color32,
    /// Hovered surface.
    pub surface_hi: Color32,
    /// Hairlines and dividers.
    pub outline: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub accent: Color32,
    pub accent_hi: Color32,
    /// Second accent, used for gradients and the spectrum's high end.
    pub accent_alt: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
}

pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(13, 14, 18),
    surface: Color32::from_rgb(22, 24, 30),
    surface_hi: Color32::from_rgb(31, 34, 42),
    outline: Color32::from_rgb(44, 48, 58),
    text: Color32::from_rgb(237, 239, 244),
    text_dim: Color32::from_rgb(154, 160, 175),
    text_faint: Color32::from_rgb(103, 109, 124),
    accent: Color32::from_rgb(255, 122, 26),
    accent_hi: Color32::from_rgb(255, 158, 74),
    accent_alt: Color32::from_rgb(255, 61, 104),
    ok: Color32::from_rgb(64, 208, 138),
    warn: Color32::from_rgb(240, 184, 64),
    err: Color32::from_rgb(255, 96, 96),
};

pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(247, 247, 250),
    surface: Color32::from_rgb(255, 255, 255),
    surface_hi: Color32::from_rgb(240, 241, 245),
    outline: Color32::from_rgb(220, 222, 230),
    text: Color32::from_rgb(24, 26, 32),
    text_dim: Color32::from_rgb(92, 98, 112),
    text_faint: Color32::from_rgb(140, 146, 160),
    accent: Color32::from_rgb(232, 96, 8),
    accent_hi: Color32::from_rgb(255, 130, 40),
    accent_alt: Color32::from_rgb(226, 40, 86),
    ok: Color32::from_rgb(24, 160, 98),
    warn: Color32::from_rgb(190, 132, 20),
    err: Color32::from_rgb(208, 56, 56),
};

impl Mode {
    pub fn palette(self) -> Palette {
        match self {
            Mode::Dark => DARK,
            Mode::Light => LIGHT,
        }
    }
}

/// Spacing scale. Using a scale rather than ad-hoc numbers is what keeps a
/// dense UI from looking accidental.
pub mod space {
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 14.0;
    pub const LG: f32 = 22.0;
}

pub mod radius {
    use egui::CornerRadius;
    pub const SM: CornerRadius = CornerRadius::same(6);
    pub const MD: CornerRadius = CornerRadius::same(10);
    pub const LG: CornerRadius = CornerRadius::same(16);
    pub const PILL: CornerRadius = CornerRadius::same(99);
}

/// Build the egui style for a mode.
pub fn style(mode: Mode) -> Style {
    let p = mode.palette();
    let mut visuals = match mode {
        Mode::Dark => Visuals::dark(),
        Mode::Light => Visuals::light(),
    };

    visuals.panel_fill = p.bg;
    visuals.window_fill = p.surface;
    visuals.extreme_bg_color = if mode == Mode::Dark {
        Color32::from_rgb(9, 10, 13)
    } else {
        Color32::from_rgb(238, 239, 243)
    };
    visuals.faint_bg_color = p.surface_hi;
    visuals.override_text_color = Some(p.text);
    visuals.hyperlink_color = p.accent;
    visuals.selection.bg_fill = p.accent.gamma_multiply(0.35);
    visuals.selection.stroke = Stroke::new(1.0, p.accent_hi);

    visuals.widgets.noninteractive.bg_fill = p.surface;
    visuals.widgets.noninteractive.weak_bg_fill = p.surface;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.outline);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text_dim);
    visuals.widgets.noninteractive.corner_radius = radius::MD;

    visuals.widgets.inactive.bg_fill = p.surface_hi;
    visuals.widgets.inactive.weak_bg_fill = p.surface;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, p.outline);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
    visuals.widgets.inactive.corner_radius = radius::MD;

    visuals.widgets.hovered.bg_fill = p.surface_hi;
    visuals.widgets.hovered.weak_bg_fill = p.surface_hi;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, p.accent.gamma_multiply(0.6));
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, p.text);
    visuals.widgets.hovered.corner_radius = radius::MD;

    visuals.widgets.active.bg_fill = p.accent.gamma_multiply(0.25);
    visuals.widgets.active.weak_bg_fill = p.accent.gamma_multiply(0.25);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, p.text);
    visuals.widgets.active.corner_radius = radius::MD;

    visuals.window_corner_radius = radius::LG;
    visuals.window_stroke = Stroke::new(1.0, p.outline);
    visuals.popup_shadow.color = Color32::from_black_alpha(96);
    visuals.window_shadow.color = Color32::from_black_alpha(120);

    let mut style = Style {
        visuals,
        ..Default::default()
    };
    style.spacing.item_spacing = egui::vec2(space::SM, space::SM);
    style.spacing.button_padding = egui::vec2(space::MD, space::SM);
    style.spacing.window_margin = Margin::same(space::MD as i8);
    style.spacing.scroll.bar_width = 8.0;
    style.spacing.slider_width = 180.0;
    style.spacing.interact_size.y = 26.0;

    // Everything in the app leans on animate_* helpers; this is their default
    // duration. Long enough to read as motion, short enough not to feel laggy.
    style.animation_time = 0.18;

    style
}

/// Apply the palette and text styles for a mode.
///
/// egui 0.36 keeps a separate `Style` per theme and picks between them from
/// `ThemePreference`, so both must be registered and then one selected —
/// mutating a single global style no longer works.
pub fn apply(ctx: &egui::Context, mode: Mode) {
    ctx.set_style_of(egui::Theme::Dark, style(Mode::Dark));
    ctx.set_style_of(egui::Theme::Light, style(Mode::Light));
    ctx.set_theme(match mode {
        Mode::Dark => egui::ThemePreference::Dark,
        Mode::Light => egui::ThemePreference::Light,
    });
    install_text_styles(ctx);
}

/// Text styles, applied to every theme's style.
pub fn install_text_styles(ctx: &egui::Context) {
    use egui::{FontFamily::Proportional, FontId, TextStyle};
    let text_styles: std::collections::BTreeMap<TextStyle, FontId> = [
        (TextStyle::Heading, FontId::new(23.0, Proportional)),
        (TextStyle::Body, FontId::new(14.0, Proportional)),
        (TextStyle::Button, FontId::new(14.0, Proportional)),
        (TextStyle::Small, FontId::new(11.5, Proportional)),
        (
            TextStyle::Monospace,
            FontId::new(13.0, egui::FontFamily::Monospace),
        ),
        (
            TextStyle::Name("title".into()),
            FontId::new(30.0, Proportional),
        ),
        (
            TextStyle::Name("hero".into()),
            FontId::new(40.0, Proportional),
        ),
    ]
    .into();
    ctx.all_styles_mut(move |style| {
        style.text_styles = text_styles.clone();
    });
}

/// Blend two colours in gamma space.
pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        f(a.r(), b.r()),
        f(a.g(), b.g()),
        f(a.b(), b.b()),
        f(a.a(), b.a()),
    )
}

/// A colour sampled from the accent gradient, `t` in 0..1.
pub fn accent_ramp(p: &Palette, t: f32) -> Color32 {
    lerp_color(p.accent, p.accent_alt, t.clamp(0.0, 1.0))
}

/// The standard card frame.
pub fn card(p: &Palette, hovered_t: f32) -> egui::Frame {
    egui::Frame::new()
        .fill(lerp_color(p.surface, p.surface_hi, hovered_t))
        .stroke(Stroke::new(
            1.0,
            lerp_color(p.outline, p.accent.gamma_multiply(0.7), hovered_t),
        ))
        .corner_radius(radius::MD)
        .inner_margin(Margin::same(space::MD as i8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_modes_produce_a_style() {
        for mode in [Mode::Dark, Mode::Light] {
            let s = style(mode);
            assert!(s.animation_time > 0.0);
            assert_eq!(s.visuals.panel_fill, mode.palette().bg);
        }
    }

    #[test]
    fn mode_toggles_round_trip() {
        assert_eq!(Mode::Dark.toggled().toggled(), Mode::Dark);
        assert_eq!(Mode::Light.toggled(), Mode::Dark);
    }

    #[test]
    fn lerp_hits_both_endpoints() {
        let a = Color32::from_rgb(0, 0, 0);
        let b = Color32::from_rgb(255, 255, 255);
        assert_eq!(lerp_color(a, b, 0.0), a);
        assert_eq!(lerp_color(a, b, 1.0), b);
        // And is clamped rather than extrapolating.
        assert_eq!(lerp_color(a, b, 3.0), b);
        assert_eq!(lerp_color(a, b, -2.0), a);
    }

    #[test]
    fn accent_ramp_is_continuous_at_the_ends() {
        let p = DARK;
        assert_eq!(accent_ramp(&p, 0.0), p.accent);
        assert_eq!(accent_ramp(&p, 1.0), p.accent_alt);
    }

    #[test]
    fn dark_and_light_have_readable_contrast() {
        // Guards against a palette edit that makes text vanish into the surface.
        let luma =
            |c: Color32| 0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32;
        for p in [DARK, LIGHT] {
            let diff = (luma(p.text) - luma(p.surface)).abs();
            assert!(diff > 90.0, "text/surface contrast too low: {diff}");
        }
    }
}
