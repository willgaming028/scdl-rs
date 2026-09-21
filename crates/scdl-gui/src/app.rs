//! The eframe application: layout, views, and per-frame animation.

use std::path::PathBuf;
use std::time::Instant;

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Id, Layout, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, TextureOptions, Ui, Vec2,
};

use scdl_core::client::ClientConfig;
use scdl_core::pipeline::DownloadOptions;
use scdl_core::resolve::Selector;
use scdl_core::stream::FormatPreferences;
use scdl_core::tag::{artwork_url, TagOptions};

use crate::anim::{self, ease, Particles, Spectrum};
use crate::bridge::{Bridge, Command, Update};
use crate::icons;
use crate::state::{AppState, ItemState, LogLevel, ToastKind, View};
use crate::theme::{self, lerp_color, radius, space, Palette};

/// How long the staggered entry animation of a result card takes.
const CARD_IN: f32 = 0.42;
/// Delay between successive cards in a staggered reveal.
const CARD_STAGGER: f32 = 0.035;

pub struct ScdlApp {
    pub state: AppState,
    bridge: Bridge,
    particles: Particles,
    spectrum: Spectrum,
    started: Instant,
    /// Frames rendered, used by the screenshot harness.
    pub frame: u64,
    /// When set, save a screenshot to this path and exit.
    pub screenshot: Option<PathBuf>,
    screenshot_requested: bool,
}

impl ScdlApp {
    pub fn new(cc: &eframe::CreationContext<'_>, output_dir: PathBuf, demo: bool) -> Self {
        let state = AppState::new(output_dir);
        theme::apply(&cc.egui_ctx, state.mode);
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let bridge = Bridge::spawn(cc.egui_ctx.clone(), ClientConfig::default());

        let mut app = Self {
            state,
            bridge,
            particles: Particles::new(110),
            spectrum: Spectrum::new(84),
            started: Instant::now(),
            frame: 0,
            screenshot: None,
            screenshot_requested: false,
        };
        if demo {
            app.seed_demo();
        }
        app
    }

    /// Deterministic state for screenshots and for seeing the UI without a
    /// network. Nothing here touches SoundCloud.
    pub fn seed_demo(&mut self) {
        use scdl_core::pipeline::Event;

        let demo: [(&str, &str, u64, u64); 7] = [
            ("Milky Way", "pandadub", 5_400_121, 5_400_121),
            ("Mayd Hubb Meets Pilgrim", "pandadub", 3_100_000, 6_200_000),
            ("Feeling Alive", "pandadub", 1_250_000, 5_010_000),
            ("Lost Reality", "pandadub", 0, 4_800_000),
            ("Planet Pillow", "pandadub", 4_200_000, 4_200_000),
            ("Purple Trip", "pandadub", 0, 0),
            ("Unknown Attack", "pandadub", 0, 0),
        ];

        for (i, (title, artist, done, total)) in demo.iter().enumerate() {
            self.state.apply_event(Event::Queued {
                index: i,
                id: 1000 + i as i64,
                title: (*title).into(),
                artist: (*artist).into(),
            });

            match i {
                // Two completed.
                0 | 4 => self.state.apply_event(Event::Finished {
                    index: i,
                    path: PathBuf::from(format!("/home/you/Music/{title}.m4a")),
                    bytes: *total,
                }),
                // Three mid-flight at fixed progress.
                1 | 2 => {
                    self.state.apply_event(Event::Started {
                        index: i,
                        path: PathBuf::from(format!("/home/you/Music/{title}.m4a")),
                        format: "hls_aac".into(),
                    });
                    self.state.apply_event(Event::Progress {
                        index: i,
                        downloaded: *done,
                        total: Some(*total),
                    });
                }
                3 => self.state.apply_event(Event::Resolving { index: i }),
                // One failed, to show the error styling.
                5 => self.state.apply_event(Event::Failed {
                    index: i,
                    error: "track is DRM-protected and cannot be downloaded".into(),
                }),
                _ => {}
            }
        }

        // Pre-fill the springs so a screenshot on an early frame shows the rings
        // at their real values rather than mid-animation from zero.
        for item in &mut self.state.queue {
            if let Some(f) = item.target_fraction() {
                item.progress.value = f;
            }
            item.speed.value = 1_450_000.0;
        }
        for (i, v) in self.state.speed_history.iter_mut().enumerate() {
            let x = i as f32 * 0.09;
            *v = (1.6e6 + (x.sin() * 0.55 + (x * 2.3).sin() * 0.3) * 9.0e5).max(0.0);
        }

        self.state.results_label = "The Lost Ship — 10 tracks".into();
        self.state.log(LogLevel::Info, "downloading into ~/Music");
        self.state
            .log(LogLevel::Success, "pandadub — Planet Pillow  ->  ~/Music");
    }

    fn palette(&self) -> Palette {
        self.state.mode.palette()
    }

    fn pump(&mut self, ctx: &egui::Context) {
        for update in self.bridge.drain() {
            match update {
                Update::Resolving => self.state.resolving = true,
                Update::Resolved { label, tracks } => {
                    self.state.resolving = false;
                    let n = tracks.len();
                    if n == 0 {
                        self.state
                            .toast(ToastKind::Warn, format!("{label}: nothing found"));
                    } else {
                        self.state.set_results(label.clone(), tracks);
                        self.state
                            .toast(ToastKind::Info, format!("{label} — {n} track(s)"));
                        self.request_missing_art();
                    }
                }
                Update::ResolveFailed(e) => {
                    self.state.resolving = false;
                    self.state.toast(ToastKind::Error, e);
                }
                Update::Pipeline(ev) => self.state.apply_event(ev),
                Update::Note(t) => self.state.log(LogLevel::Info, t),
                Update::Error(t) => self.state.toast(ToastKind::Error, t),
                Update::Art {
                    track_id,
                    rgba,
                    width,
                    height,
                } => {
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [width as usize, height as usize],
                        &rgba,
                    );
                    let handle =
                        ctx.load_texture(format!("art-{track_id}"), image, TextureOptions::LINEAR);
                    self.state.art.insert(track_id, Some(handle));
                }
            }
        }
    }

    /// Ask the worker for any cover art we do not have yet.
    fn request_missing_art(&mut self) {
        let wanted: Vec<(i64, String)> = self
            .state
            .results
            .iter()
            .filter(|r| !self.state.art.contains_key(&r.track.id))
            .filter_map(|r| artwork_url(&r.track, false).map(|u| (r.track.id, u)))
            .take(60)
            .collect();

        for (track_id, url) in wanted {
            // Insert a placeholder so the same art is not requested twice.
            self.state.art.insert(track_id, None);
            self.bridge.send(Command::FetchArt { track_id, url });
        }
    }

    fn start_resolve(&mut self) {
        let q = self.state.query.trim().to_string();
        if q.is_empty() {
            return;
        }
        self.state.resolving = true;
        self.state.log(LogLevel::Info, format!("resolving {q}"));
        self.bridge.send(Command::Resolve {
            input: q,
            selector: self.state.selector,
        });
    }

    fn start_download(&mut self) {
        let tracks = self.state.selected_tracks();
        if tracks.is_empty() {
            self.state.toast(ToastKind::Warn, "nothing selected");
            return;
        }
        let n = tracks.len();

        let options = DownloadOptions {
            output_dir: self.state.output_dir.clone(),
            name_format: self.state.name_format.clone(),
            playlist_name_format: self.state.playlist_name_format.clone(),
            format_prefs: FormatPreferences {
                only_mp3: self.state.only_mp3,
                allow_opus: self.state.allow_opus,
                ..Default::default()
            },
            tag_opts: TagOptions::default(),
            concurrency: self.state.concurrency,
            ..Default::default()
        };

        let archive_path = self
            .state
            .use_archive
            .then(|| self.state.output_dir.join(".scdl-archive.txt"));

        self.bridge.send(Command::Download {
            tracks,
            options: Box::new(options),
            archive_path,
        });
        self.state.view = View::Queue;
        self.state
            .toast(ToastKind::Info, format!("queued {n} track(s)"));
    }
}

impl eframe::App for ScdlApp {
    // egui 0.36 hands the app a root `Ui` rather than a `Context`; panels are
    // nested inside it instead of being registered against the context.
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        self.frame += 1;
        self.pump(ctx);
        self.state.prune_toasts();
        self.state.tick_history();

        let p = self.palette();
        let time = ctx.input(|i| i.time) as f32;
        let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);

        // Advance every per-item spring once per frame.
        for item in &mut self.state.queue {
            let target = item.target_fraction().unwrap_or(item.progress.value);
            item.progress.step(target, dt);
        }

        // Repaint continuously only while something moves; otherwise idle and
        // let egui sleep, which keeps the GPU quiet.
        if self.state.animations_on && (self.state.needs_animation() || self.frame < 10) {
            ctx.request_repaint();
        }

        self.draw_background(ctx, &p, time, dt);
        self.draw_nav(ui, &p);
        self.draw_status_bar(ui, &p, time);

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::same(space::LG as i8)))
            .show(ui, |ui| match self.state.view {
                View::Browse => self.view_browse(ui, &p, time),
                View::Queue => self.view_queue(ui, &p, time),
                View::Library => self.view_library(ui, &p),
                View::Settings => self.view_settings(ui, &p),
            });

        self.draw_toasts(ctx, &p);
        self.handle_screenshot(ctx);
    }
}

// ------------------------------------------------------------- chrome ------

impl ScdlApp {
    fn draw_background(&mut self, ctx: &egui::Context, p: &Palette, time: f32, dt: f32) {
        let screen = ctx.content_rect();
        // A background *layer* rather than an Area: an Area participates in
        // layout and interaction, and a full-screen one sits on top of the
        // panels and hides them.
        let painter = ctx.layer_painter(egui::LayerId::background());
        painter.rect_filled(screen, CornerRadius::ZERO, p.bg);
        if self.state.animations_on {
            anim::aurora(&painter, time, screen, p, 1.0);
            self.particles
                .update_and_draw(&painter, dt, screen, p, 0.55);
        }
    }

    fn draw_nav(&mut self, root: &mut Ui, p: &Palette) {
        egui::Panel::left(Id::new("nav"))
            .exact_size(74.0)
            .resizable(false)
            .frame(
                egui::Frame::NONE
                    .fill(p.surface.gamma_multiply(0.82))
                    .inner_margin(egui::Margin::symmetric(0, space::MD as i8)),
            )
            .show(root, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(space::SM);
                    // Wordmark: a small animated equalizer next to the name.
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(40.0, 26.0), Sense::hover());
                    let t = ui.input(|i| i.time) as f32;
                    icons::equalizer(ui.painter(), rect, t, self.state.active_count() > 0, |x| {
                        theme::accent_ramp(p, x)
                    });
                    ui.add_space(space::LG);

                    let mut clicked = None;
                    // Track where the active pill should sit so it can slide.
                    let mut active_y = None;

                    for view in View::ALL {
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::new(52.0, 46.0), Sense::click());
                        let is_active = self.state.view == view;
                        if is_active {
                            active_y = Some(rect.center().y);
                        }

                        let hover_t = ui.ctx().animate_bool_with_time(
                            resp.id.with("hover"),
                            resp.hovered(),
                            0.14,
                        );

                        if resp.hovered() && !is_active {
                            ui.painter().rect_filled(
                                rect.shrink(4.0),
                                radius::MD,
                                p.surface_hi.gamma_multiply(hover_t * 0.9),
                            );
                        }

                        let color = if is_active {
                            p.accent
                        } else {
                            lerp_color(p.text_faint, p.text, hover_t)
                        };
                        let ic = rect.center() - Vec2::new(0.0, 5.0);
                        match view {
                            View::Browse => icons::search(ui.painter(), ic, 22.0, color),
                            View::Queue => icons::download(ui.painter(), ic, 22.0, color),
                            View::Library => icons::note(ui.painter(), ic, 22.0, color),
                            View::Settings => icons::gear(ui.painter(), ic, 22.0, color),
                        }
                        ui.painter().text(
                            rect.center() + Vec2::new(0.0, 13.0),
                            Align2::CENTER_CENTER,
                            view.label(),
                            FontId::proportional(9.5),
                            color.gamma_multiply(0.9),
                        );

                        if resp.clicked() {
                            clicked = Some(view);
                        }
                        resp.on_hover_text(view.label());
                    }

                    // The sliding active indicator: one pill animated toward the
                    // active row rather than four that blink on and off.
                    if let Some(target_y) = active_y {
                        let y =
                            ui.ctx()
                                .animate_value_with_time(Id::new("nav_pill"), target_y, 0.22);
                        let x = ui.max_rect().left() + 3.0;
                        let pill =
                            Rect::from_min_size(Pos2::new(x, y - 15.0), Vec2::new(3.5, 30.0));
                        ui.painter().rect_filled(pill, radius::PILL, p.accent);
                        anim::glow_rect(ui.painter(), pill, radius::PILL, p.accent, 1.0);
                    }

                    if let Some(v) = clicked {
                        self.state.view = v;
                    }

                    // Theme toggle pinned to the bottom.
                    let avail = ui.available_height();
                    if avail > 44.0 {
                        ui.add_space(avail - 40.0);
                    }
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(40.0, 32.0), Sense::click());
                    let t = ui.ctx().animate_bool(Id::new("theme_btn"), resp.hovered());
                    let c = lerp_color(p.text_faint, p.accent, t);
                    if self.state.mode == theme::Mode::Dark {
                        icons::moon(ui.painter(), rect.center(), 20.0, c, p.surface);
                    } else {
                        icons::sun(ui.painter(), rect.center(), 20.0, c);
                    }
                    if resp.clicked() {
                        self.state.mode = self.state.mode.toggled();
                        theme::apply(ui.ctx(), self.state.mode);
                    }
                    resp.on_hover_text("Toggle theme");
                });
            });
    }

    fn draw_status_bar(&mut self, root: &mut Ui, p: &Palette, _time: f32) {
        egui::Panel::bottom(Id::new("status"))
            .exact_size(30.0)
            .frame(
                egui::Frame::NONE
                    .fill(p.surface.gamma_multiply(0.9))
                    .inner_margin(egui::Margin::symmetric(space::MD as i8, 0)),
            )
            .show(root, |ui| {
                ui.horizontal_centered(|ui| {
                    let active = self.state.active_count();
                    let speed = self.state.aggregate_speed();

                    if active > 0 {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                        let t = ui.input(|i| i.time) as f32;
                        anim::spinner(ui.painter(), t, r.center(), 6.0, p);
                        ui.label(
                            RichText::new(format!("{active} active"))
                                .color(p.text)
                                .size(12.0),
                        );
                        ui.label(
                            RichText::new(format!("{}/s", anim::human_bytes(speed as u64)))
                                .color(p.accent)
                                .size(12.0),
                        );
                    } else {
                        ui.label(RichText::new("idle").color(p.text_faint).size(12.0));
                    }

                    ui.separator();
                    ui.label(
                        RichText::new(format!(
                            "{} done · {} skipped · {} failed · {}",
                            self.state.completed,
                            self.state.skipped,
                            self.state.failed,
                            anim::human_bytes(self.state.total_bytes)
                        ))
                        .color(p.text_dim)
                        .size(12.0),
                    );

                    // Overall run progress, tweened so it glides rather than
                    // stepping as each track completes.
                    if !self.state.queue.is_empty() {
                        let target = self.state.overall_fraction();
                        let v = ui.ctx().animate_value_with_time(
                            Id::new("overall_progress"),
                            target,
                            0.35,
                        );
                        let (bar, _) =
                            ui.allocate_exact_size(Vec2::new(150.0, 6.0), Sense::hover());
                        ui.painter().rect_filled(bar, radius::PILL, p.outline);
                        ui.painter().rect_filled(
                            Rect::from_min_size(
                                bar.min,
                                Vec2::new(bar.width() * v.clamp(0.0, 1.0), bar.height()),
                            ),
                            radius::PILL,
                            p.accent,
                        );
                        ui.label(
                            RichText::new(format!("{:.0}%", v * 100.0))
                                .color(p.text_dim)
                                .size(11.5),
                        );
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(self.state.output_dir.display().to_string())
                                .color(p.text_faint)
                                .size(11.5),
                        );
                    });
                });
            });
    }

    fn draw_toasts(&mut self, ctx: &egui::Context, p: &Palette) {
        let screen = ctx.content_rect();
        let toasts = self.state.toasts.clone();

        egui::Area::new(Id::new("toasts"))
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::new(
                screen.right() - 360.0,
                (screen.bottom() - 52.0 - toasts.len().max(1) as f32 * 54.0)
                    .max(screen.top() + 12.0),
            ))
            .show(ctx, |ui| {
                ui.set_width(344.0);
                let base = ui.available_rect_before_wrap();
                for (i, toast) in toasts.iter().enumerate() {
                    let age = toast.age();
                    // Slide + fade in, then out at the end of its life.
                    let appear = ease::out_back((age / 0.35).clamp(0.0, 1.0));
                    let leaving = ((age - (toast.lifetime - 0.35)) / 0.35).clamp(0.0, 1.0);
                    let alpha = (1.0 - leaving) * ease::smoothstep(age / 0.2);
                    let dx = (1.0 - appear) * 60.0 + leaving * 60.0;

                    let accent = match toast.kind {
                        ToastKind::Info => p.accent,
                        ToastKind::Success => p.ok,
                        ToastKind::Warn => p.warn,
                        ToastKind::Error => p.err,
                    };

                    let height = 46.0;
                    let rect = Rect::from_min_size(
                        Pos2::new(base.left() + dx, base.top() + i as f32 * (height + 8.0)),
                        Vec2::new(base.width(), height),
                    );

                    let painter = ui.painter();
                    painter.rect_filled(rect, radius::MD, p.surface.gamma_multiply(alpha * 0.97));
                    painter.rect_stroke(
                        rect,
                        radius::MD,
                        Stroke::new(1.0, accent.gamma_multiply(alpha * 0.55)),
                        StrokeKind::Inside,
                    );
                    // Accent bar down the left edge.
                    painter.rect_filled(
                        Rect::from_min_size(
                            rect.min + Vec2::new(0.0, 6.0),
                            Vec2::new(3.0, height - 12.0),
                        ),
                        radius::PILL,
                        accent.gamma_multiply(alpha),
                    );
                    painter.text(
                        rect.min + Vec2::new(14.0, height * 0.5),
                        Align2::LEFT_CENTER,
                        truncate(&toast.text, 46),
                        FontId::proportional(13.0),
                        p.text.gamma_multiply(alpha),
                    );
                    // Timer hairline along the bottom.
                    let w = rect.width() * toast.remaining();
                    painter.rect_filled(
                        Rect::from_min_size(
                            Pos2::new(rect.left(), rect.bottom() - 2.0),
                            Vec2::new(w, 2.0),
                        ),
                        radius::PILL,
                        accent.gamma_multiply(alpha * 0.5),
                    );
                }
                // Reserve the stack's full height once, after the loop.
                let n = toasts.len() as f32;
                if n > 0.0 {
                    ui.advance_cursor_after_rect(Rect::from_min_size(
                        base.min,
                        Vec2::new(base.width(), n * 54.0),
                    ));
                }
            });
    }
}

// -------------------------------------------------------------- views ------

impl ScdlApp {
    fn view_browse(&mut self, ui: &mut Ui, p: &Palette, time: f32) {
        ui.label(
            RichText::new("Browse")
                .font(FontId::proportional(30.0))
                .color(p.text),
        );
        ui.add_space(space::SM);

        // --- search bar ---
        let bar_h = 46.0;
        let (bar_rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), bar_h), Sense::hover());
        let focus_t = ui
            .ctx()
            .animate_bool(Id::new("search_focus"), !self.state.query.is_empty());

        ui.painter().rect_filled(
            bar_rect,
            radius::PILL,
            lerp_color(p.surface, p.surface_hi, focus_t),
        );
        ui.painter().rect_stroke(
            bar_rect,
            radius::PILL,
            Stroke::new(1.0, lerp_color(p.outline, p.accent, focus_t)),
            StrokeKind::Inside,
        );
        if focus_t > 0.01 {
            anim::glow_rect(ui.painter(), bar_rect, radius::PILL, p.accent, focus_t);
        }

        let mut submit = false;
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(bar_rect.shrink2(Vec2::new(space::MD, 8.0))),
            |ui| {
                ui.horizontal_centered(|ui| {
                    let (gr, _) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
                    icons::search(ui.painter(), gr.center(), 19.0, p.accent);
                    let edit = egui::TextEdit::singleline(&mut self.state.query)
                        .hint_text("Paste a SoundCloud URL, or search…")
                        .desired_width(ui.available_width() - 190.0)
                        .frame(egui::Frame::NONE)
                        .font(FontId::proportional(15.0));
                    let resp = ui.add(edit);
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = true;
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if pill_button(ui, p, "Resolve", true).clicked() {
                            submit = true;
                        }
                        if self.state.resolving {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                            let t = ui.input(|i| i.time) as f32;
                            anim::spinner(ui.painter(), t, r.center(), 8.0, p);
                        }
                    });
                });
            },
        );
        if submit {
            self.start_resolve();
        }

        ui.add_space(space::SM);

        // --- selector chips ---
        ui.horizontal(|ui| {
            ui.label(RichText::new("From a user:").color(p.text_dim).size(12.5));
            for (sel, label) in [
                (Selector::Tracks, "Uploads"),
                (Selector::All, "All + reposts"),
                (Selector::Likes, "Likes"),
                (Selector::Playlists, "Playlists"),
                (Selector::Reposts, "Reposts"),
                (Selector::Comments, "Commented"),
            ] {
                let active = self.state.selector == sel;
                if chip(ui, p, label, active).clicked() {
                    self.state.selector = sel;
                }
            }
        });

        ui.add_space(space::MD);

        // --- results ---
        if self.state.results.is_empty() {
            self.draw_empty_browse(ui, p, time);
            return;
        }

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&self.state.results_label)
                    .size(15.0)
                    .color(p.text),
            );
            ui.label(
                RichText::new(format!(
                    "· {} of {} selected",
                    self.state.selected_count(),
                    self.state.results.len()
                ))
                .color(p.text_dim)
                .size(13.0),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let n = self.state.selected_count();
                if pill_button(ui, p, &format!("Download {n}"), n > 0).clicked() {
                    self.start_download();
                }
                if ghost_button(ui, p, "None").clicked() {
                    self.state.select_all(false);
                }
                if ghost_button(ui, p, "All").clicked() {
                    self.state.select_all(true);
                }
            });
        });

        ui.add_space(space::SM);

        let mut toggled = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for idx in 0..self.state.results.len() {
                    if self.draw_result_card(ui, p, idx, time) {
                        toggled = Some(idx);
                    }
                }
            });
        if let Some(i) = toggled {
            self.state.results[i].selected = !self.state.results[i].selected;
        }
    }

    fn draw_empty_browse(&self, ui: &mut Ui, p: &Palette, time: f32) {
        let rect = ui.available_rect_before_wrap();
        let center = rect.center();

        // A slow breathing ring as a focal point.
        let pulse = (time * 0.9).sin() * 0.5 + 0.5;
        anim::progress_ring(
            ui.painter(),
            time,
            anim::Ring {
                center: center - Vec2::new(0.0, 40.0),
                radius: 42.0 + pulse * 4.0,
                thickness: 4.0,
                progress: None,
                label: None,
            },
            p,
        );
        ui.painter().text(
            center + Vec2::new(0.0, 26.0),
            Align2::CENTER_CENTER,
            "Paste a SoundCloud link to begin",
            FontId::proportional(16.0),
            p.text_dim,
        );
        ui.painter().text(
            center + Vec2::new(0.0, 50.0),
            Align2::CENTER_CENTER,
            "a track, a playlist, or a whole user profile",
            FontId::proportional(12.5),
            p.text_faint,
        );
    }

    /// Returns true when the card was clicked.
    fn draw_result_card(&self, ui: &mut Ui, p: &Palette, idx: usize, _time: f32) -> bool {
        let item = &self.state.results[idx];
        let elapsed = self.started.elapsed().as_secs_f32();

        // Staggered entry: each card starts slightly after the one above it.
        let delay = item.ordinal as f32 * CARD_STAGGER;
        let t = ease::out_quint(((elapsed - delay) / CARD_IN).clamp(0.0, 1.0));

        let h = 62.0;
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::click());
        if t <= 0.001 {
            return false;
        }

        let hover_t = ui
            .ctx()
            .animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.13);
        // Slide up and fade in, and lift slightly on hover.
        let offset = Vec2::new(0.0, (1.0 - t) * 18.0 - hover_t * 2.0);
        let rect = rect.translate(offset);
        let alpha = t;

        let painter = ui.painter();
        painter.rect_filled(
            rect,
            radius::MD,
            lerp_color(p.surface, p.surface_hi, hover_t).gamma_multiply(alpha * 0.96),
        );
        painter.rect_stroke(
            rect,
            radius::MD,
            Stroke::new(
                1.0,
                if item.selected {
                    p.accent.gamma_multiply(alpha * 0.8)
                } else {
                    p.outline.gamma_multiply(alpha)
                },
            ),
            StrokeKind::Inside,
        );

        // Cover art, or a shimmer while it loads.
        let art_rect = Rect::from_min_size(rect.min + Vec2::new(8.0, 8.0), Vec2::splat(h - 16.0));
        match self.state.art.get(&item.track.id) {
            Some(Some(tex)) => {
                painter.image(
                    tex.id(),
                    art_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE.gamma_multiply(alpha),
                );
            }
            _ => anim::shimmer(ui.painter(), ui.input(|i| i.time) as f32, art_rect, p),
        }

        // Selection tick.
        let tick_c = Pos2::new(rect.right() - 26.0, rect.center().y);
        let sel_t = ui
            .ctx()
            .animate_bool_with_time(resp.id.with("sel"), item.selected, 0.16);
        painter.circle_stroke(
            tick_c,
            9.0,
            Stroke::new(
                1.5,
                lerp_color(p.text_faint, p.accent, sel_t).gamma_multiply(alpha),
            ),
        );
        if sel_t > 0.01 {
            painter.circle_filled(
                tick_c,
                9.0 * ease::out_elastic(sel_t),
                p.accent.gamma_multiply(alpha * sel_t),
            );
            icons::check(
                painter,
                tick_c,
                14.0 * sel_t,
                Color32::from_rgb(20, 14, 8).gamma_multiply(alpha * sel_t),
            );
        }

        let text_x = art_rect.right() + 12.0;
        painter.text(
            Pos2::new(text_x, rect.center().y - 9.0),
            Align2::LEFT_CENTER,
            truncate(item.track.title_or_untitled(), 54),
            FontId::proportional(14.5),
            p.text.gamma_multiply(alpha),
        );
        painter.text(
            Pos2::new(text_x, rect.center().y + 11.0),
            Align2::LEFT_CENTER,
            truncate(item.track.artist(), 42),
            FontId::proportional(12.0),
            p.text_dim.gamma_multiply(alpha),
        );
        if let Some(secs) = item.track.duration_secs() {
            painter.text(
                Pos2::new(rect.right() - 48.0, rect.center().y),
                Align2::RIGHT_CENTER,
                anim::human_duration(secs as f32),
                FontId::proportional(12.0),
                p.text_faint.gamma_multiply(alpha),
            );
        }

        ui.add_space(6.0);
        resp.clicked()
    }

    fn view_queue(&mut self, ui: &mut Ui, p: &Palette, time: f32) {
        ui.label(
            RichText::new("Queue")
                .font(FontId::proportional(30.0))
                .color(p.text),
        );
        ui.add_space(space::SM);

        if self.state.queue.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("Nothing queued yet")
                        .color(p.text_faint)
                        .size(15.0),
                );
            });
            return;
        }

        self.draw_hero(ui, p, time);
        ui.add_space(space::MD);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt("queue_rows")
            .max_height(ui.available_height() - 150.0)
            .show(ui, |ui| {
                for idx in 0..self.state.queue.len() {
                    self.draw_queue_row(ui, p, idx);
                    ui.add_space(6.0);
                }
            });

        ui.add_space(space::SM);
        self.draw_log_drawer(ui, p);
    }

    /// Recent activity, newest last. Small on purpose: the queue is the
    /// primary surface and this is for the detail behind a failure.
    fn draw_log_drawer(&self, ui: &mut Ui, p: &Palette) {
        let rect = ui.available_rect_before_wrap();
        if rect.height() < 40.0 {
            return;
        }
        ui.painter()
            .rect_filled(rect, radius::MD, p.surface.gamma_multiply(0.7));

        let inner = rect.shrink2(Vec2::new(space::MD, space::SM));
        let line_h = 16.0;
        let capacity = (inner.height() / line_h).floor().max(1.0) as usize;
        let start = self.state.logs.len().saturating_sub(capacity);

        for (row, line) in self.state.logs[start..].iter().enumerate() {
            let color = match line.level {
                LogLevel::Info => p.text_faint,
                LogLevel::Success => p.ok,
                LogLevel::Warn => p.warn,
                LogLevel::Error => p.err,
            };
            ui.painter().text(
                Pos2::new(
                    inner.left(),
                    inner.top() + row as f32 * line_h + line_h * 0.5,
                ),
                Align2::LEFT_CENTER,
                truncate(&line.text, (inner.width() / 6.6) as usize),
                FontId::proportional(11.5),
                color,
            );
        }
    }

    /// The big "now downloading" panel: art, spectrum, ring, sparkline.
    fn draw_hero(&mut self, ui: &mut Ui, p: &Palette, time: f32) {
        let hero_h = 148.0;
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), hero_h), Sense::hover());

        let painter = ui.painter();
        painter.rect_filled(rect, radius::LG, p.surface.gamma_multiply(0.94));
        painter.rect_stroke(
            rect,
            radius::LG,
            Stroke::new(1.0, p.outline),
            StrokeKind::Inside,
        );

        let Some(item) = self.state.hero_item().cloned() else {
            return;
        };
        let active = item.state.is_active();

        // Spectrum across the bottom, behind everything, clipped to the card.
        let spec_rect = Rect::from_min_max(
            Pos2::new(rect.left() + 10.0, rect.bottom() - 40.0),
            Pos2::new(rect.right() - 10.0, rect.bottom() - 8.0),
        );
        let mags = if active {
            self.spectrum.idle_magnitudes(time, 0.62)
        } else {
            vec![0.0; self.spectrum.len()]
        };
        let dt = ui.input(|i| i.stable_dt);
        let spec_painter = ui.painter().clone().with_clip_rect(rect.shrink(1.0));
        self.spectrum
            .update_and_draw(&spec_painter, dt, spec_rect, &mags, p);

        // Progress ring on the left.
        let ring_c = Pos2::new(rect.left() + 62.0, rect.top() + 56.0);
        let frac = if active {
            Some(item.progress.value)
        } else {
            item.target_fraction()
        };
        let label = frac.map(|f| format!("{:.0}%", f * 100.0));
        anim::progress_ring(
            ui.painter(),
            time,
            anim::Ring {
                center: ring_c,
                radius: 38.0,
                thickness: 6.0,
                progress: if item.state == ItemState::Resolving {
                    None
                } else {
                    frac
                },
                label: label.as_deref(),
            },
            p,
        );

        // Text block.
        let tx = rect.left() + 118.0;
        let painter = ui.painter();
        painter.text(
            Pos2::new(tx, rect.top() + 32.0),
            Align2::LEFT_CENTER,
            truncate(&item.title, 46),
            FontId::proportional(21.0),
            p.text,
        );
        painter.text(
            Pos2::new(tx, rect.top() + 56.0),
            Align2::LEFT_CENTER,
            truncate(&item.artist, 46),
            FontId::proportional(13.5),
            p.text_dim,
        );

        let detail = match item.state {
            ItemState::Downloading => {
                let mut s = anim::human_bytes(item.downloaded);
                if let Some(t) = item.total {
                    s = format!("{s} / {}", anim::human_bytes(t));
                }
                s = format!("{s}   {}/s", anim::human_bytes(item.speed.value as u64));
                if let Some(eta) = item.eta_secs() {
                    s = format!("{s}   ETA {}", anim::human_duration(eta));
                }
                s
            }
            _ => item
                .message
                .clone()
                .unwrap_or_else(|| item.state.label().to_string()),
        };
        painter.text(
            Pos2::new(tx, rect.top() + 80.0),
            Align2::LEFT_CENTER,
            detail,
            FontId::proportional(12.5),
            if item.state == ItemState::Failed {
                p.err
            } else {
                p.text_faint
            },
        );

        // Throughput sparkline, top-right.
        let spark = Rect::from_min_size(
            Pos2::new(rect.right() - 210.0, rect.top() + 16.0),
            Vec2::new(194.0, 46.0),
        );
        self.draw_sparkline(ui, spark, p);
    }

    fn draw_sparkline(&self, ui: &Ui, rect: Rect, p: &Palette) {
        let data = &self.state.speed_history;
        if data.len() < 2 {
            return;
        }
        let max = data.iter().cloned().fold(1.0f32, f32::max);
        let painter = ui.painter();

        let pts: Vec<Pos2> = data
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let x = rect.left() + rect.width() * (i as f32 / (data.len() - 1) as f32);
                let y = rect.bottom() - (v / max).clamp(0.0, 1.0) * rect.height();
                Pos2::new(x, y)
            })
            .collect();

        // Filled area under the curve.
        let mut mesh = egui::epaint::Mesh::default();
        for (i, pt) in pts.iter().enumerate() {
            let t = i as f32 / (pts.len() - 1) as f32;
            let c = theme::accent_ramp(p, t);
            mesh.vertices.push(egui::epaint::Vertex {
                pos: *pt,
                uv: egui::epaint::WHITE_UV,
                color: c.gamma_multiply(0.30),
            });
            mesh.vertices.push(egui::epaint::Vertex {
                pos: Pos2::new(pt.x, rect.bottom()),
                uv: egui::epaint::WHITE_UV,
                color: c.gamma_multiply(0.02),
            });
        }
        for i in 0..pts.len().saturating_sub(1) {
            let b = (i * 2) as u32;
            mesh.indices
                .extend_from_slice(&[b, b + 1, b + 2, b + 2, b + 1, b + 3]);
        }
        painter.add(egui::Shape::mesh(mesh));

        for w in pts.windows(2).enumerate() {
            let (i, seg) = w;
            let t = i as f32 / pts.len() as f32;
            painter.line_segment([seg[0], seg[1]], Stroke::new(1.6, theme::accent_ramp(p, t)));
        }

        painter.text(
            rect.left_top(),
            Align2::LEFT_TOP,
            "throughput",
            FontId::proportional(10.0),
            p.text_faint,
        );
    }

    fn draw_queue_row(&self, ui: &mut Ui, p: &Palette, idx: usize) {
        let item = &self.state.queue[idx];
        let now = ui.input(|i| i.time) as f32;
        let h = 56.0;
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());

        // Entry animation, keyed off when the item was created.
        let age = item.born.elapsed().as_secs_f32();
        let t = ease::out_cubic((age / 0.4).clamp(0.0, 1.0));
        let hover_t = ui
            .ctx()
            .animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.13);
        let rect = rect.translate(Vec2::new((1.0 - t) * 24.0, 0.0));
        let alpha = t;

        let accent = match item.state {
            ItemState::Done => p.ok,
            ItemState::Failed => p.err,
            ItemState::Skipped => p.text_faint,
            _ => p.accent,
        };

        let painter = ui.painter();
        painter.rect_filled(
            rect,
            radius::MD,
            lerp_color(p.surface, p.surface_hi, hover_t).gamma_multiply(alpha * 0.95),
        );
        painter.rect_stroke(
            rect,
            radius::MD,
            Stroke::new(1.0, p.outline.gamma_multiply(alpha)),
            StrokeKind::Inside,
        );

        // Status ring.
        let ring_c = Pos2::new(rect.left() + 30.0, rect.center().y);
        match item.state {
            ItemState::Resolving | ItemState::Tagging => {
                anim::spinner(ui.painter(), now, ring_c, 14.0, p);
            }
            ItemState::Downloading => {
                anim::progress_ring(
                    ui.painter(),
                    now,
                    anim::Ring {
                        center: ring_c,
                        radius: 15.0,
                        thickness: 3.0,
                        progress: Some(item.progress.value),
                        label: None,
                    },
                    p,
                );
            }
            _ => {
                painter.circle_stroke(
                    ring_c,
                    14.0,
                    Stroke::new(1.5, accent.gamma_multiply(alpha * 0.55)),
                );
                let c = accent.gamma_multiply(alpha);
                match item.state {
                    ItemState::Done => icons::check(painter, ring_c, 16.0, c),
                    ItemState::Failed => icons::cross(painter, ring_c, 16.0, c),
                    ItemState::Skipped => icons::dash(painter, ring_c, 16.0, c),
                    _ => icons::dot(painter, ring_c, 16.0, c),
                }
            }
        }

        let painter = ui.painter();
        let tx = rect.left() + 58.0;
        painter.text(
            Pos2::new(tx, rect.center().y - 8.0),
            Align2::LEFT_CENTER,
            truncate(&item.title, 50),
            FontId::proportional(14.0),
            p.text.gamma_multiply(alpha),
        );
        let sub = match item.state {
            ItemState::Downloading => format!(
                "{} · {}/s",
                anim::human_bytes(item.downloaded),
                anim::human_bytes(item.speed.value as u64)
            ),
            _ => item
                .message
                .clone()
                .unwrap_or_else(|| item.state.label().to_string()),
        };
        painter.text(
            Pos2::new(tx, rect.center().y + 11.0),
            Align2::LEFT_CENTER,
            truncate(&sub, 62),
            FontId::proportional(11.5),
            if item.state == ItemState::Failed {
                p.err.gamma_multiply(alpha)
            } else {
                p.text_faint.gamma_multiply(alpha)
            },
        );

        // A thin progress hairline along the bottom of the card.
        if item.state == ItemState::Downloading {
            let w = rect.width() * item.progress.value.clamp(0.0, 1.0);
            painter.rect_filled(
                Rect::from_min_size(
                    Pos2::new(rect.left(), rect.bottom() - 2.0),
                    Vec2::new(w, 2.0),
                ),
                radius::PILL,
                p.accent.gamma_multiply(alpha),
            );
        }
    }

    fn view_library(&mut self, ui: &mut Ui, p: &Palette) {
        ui.label(
            RichText::new("Library")
                .font(FontId::proportional(30.0))
                .color(p.text),
        );
        ui.label(
            RichText::new(format!(
                "{} file(s) · {}",
                self.state.library.len(),
                anim::human_bytes(self.state.total_bytes)
            ))
            .color(p.text_dim)
            .size(13.0),
        );
        ui.add_space(space::MD);

        if self.state.library.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("Nothing downloaded in this session yet")
                        .color(p.text_faint)
                        .size(15.0),
                );
            });
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let entries = self.state.library.clone();
                for e in entries {
                    let (rect, resp) = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), 46.0), Sense::click());
                    let hover_t =
                        ui.ctx()
                            .animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.12);
                    let painter = ui.painter();
                    painter.rect_filled(
                        rect,
                        radius::MD,
                        lerp_color(p.surface, p.surface_hi, hover_t).gamma_multiply(0.9),
                    );
                    // Thumbnail, if the art was fetched while browsing.
                    let art = Rect::from_min_size(
                        rect.min + Vec2::new(6.0, 6.0),
                        Vec2::splat(rect.height() - 12.0),
                    );
                    if let Some(Some(tex)) = self.state.art.get(&e.track_id) {
                        painter.image(
                            tex.id(),
                            art,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    } else {
                        painter.rect_filled(art, radius::SM, p.surface_hi);
                        icons::note(painter, art.center(), 18.0, p.text_faint);
                    }
                    painter.text(
                        rect.left_center() + Vec2::new(rect.height(), -7.0),
                        Align2::LEFT_CENTER,
                        truncate(&e.title, 54),
                        FontId::proportional(13.5),
                        p.text,
                    );
                    painter.text(
                        rect.left_center() + Vec2::new(rect.height(), 10.0),
                        Align2::LEFT_CENTER,
                        truncate(&e.artist, 40),
                        FontId::proportional(11.5),
                        p.text_dim,
                    );
                    painter.text(
                        rect.right_center() - Vec2::new(14.0, 0.0),
                        Align2::RIGHT_CENTER,
                        anim::human_bytes(e.bytes),
                        FontId::proportional(11.5),
                        p.text_faint,
                    );
                    resp.on_hover_text(e.path.display().to_string());
                    ui.add_space(5.0);
                }
            });
    }

    fn view_settings(&mut self, ui: &mut Ui, p: &Palette) {
        ui.label(
            RichText::new("Settings")
                .font(FontId::proportional(30.0))
                .color(p.text),
        );
        ui.add_space(space::MD);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                settings_group(ui, p, "Output", |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Folder").color(p.text_dim));
                        let mut s = self.state.output_dir.display().to_string();
                        if ui
                            .add_sized(
                                [ui.available_width(), 22.0],
                                egui::TextEdit::singleline(&mut s),
                            )
                            .changed()
                        {
                            self.state.output_dir = PathBuf::from(s);
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Name format").color(p.text_dim));
                        ui.add_sized(
                            [ui.available_width(), 22.0],
                            egui::TextEdit::singleline(&mut self.state.name_format),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Playlist format").color(p.text_dim));
                        ui.add_sized(
                            [ui.available_width(), 22.0],
                            egui::TextEdit::singleline(&mut self.state.playlist_name_format),
                        );
                    });
                });

                settings_group(ui, p, "Downloading", |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Parallel downloads").color(p.text_dim));
                        ui.add(egui::Slider::new(&mut self.state.concurrency, 1..=16));
                    });
                    ui.checkbox(&mut self.state.only_mp3, "MP3 only");
                    ui.checkbox(&mut self.state.allow_opus, "Allow Opus");
                    ui.checkbox(
                        &mut self.state.use_archive,
                        "Keep a download archive (skip what is already downloaded)",
                    );
                });

                settings_group(ui, p, "Account", |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("OAuth token").color(p.text_dim));
                        ui.add_sized(
                            [ui.available_width(), 22.0],
                            egui::TextEdit::singleline(&mut self.state.auth_token)
                                .password(true)
                                .hint_text("optional — for likes and private tracks"),
                        );
                    });
                    ui.label(
                        RichText::new(
                            "Stored only in memory here; the CLI reads ~/.config/scdl/scdl.cfg",
                        )
                        .color(p.text_faint)
                        .size(11.5),
                    );
                });

                settings_group(ui, p, "Appearance", |ui| {
                    ui.checkbox(&mut self.state.animations_on, "Animations");
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Theme").color(p.text_dim));
                        if ui
                            .button(if self.state.mode == theme::Mode::Dark {
                                "Dark"
                            } else {
                                "Light"
                            })
                            .clicked()
                        {
                            self.state.mode = self.state.mode.toggled();
                            theme::apply(ui.ctx(), self.state.mode);
                        }
                    });
                });
            });
    }
}

// --------------------------------------------------------- screenshots -----

impl ScdlApp {
    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot.clone() else {
            return;
        };

        // Give the UI a few frames so fonts, textures and entry animations have
        // settled, then ask egui for its own framebuffer.
        if !self.screenshot_requested && self.frame >= 12 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            self.screenshot_requested = true;
        }
        ctx.request_repaint();

        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });

        if let Some(image) = shot {
            let [w, h] = image.size;
            let rgba: Vec<u8> = image
                .pixels
                .iter()
                .flat_map(|c| c.to_srgba_unmultiplied())
                .collect();
            match image::RgbaImage::from_raw(w as u32, h as u32, rgba) {
                Some(buf) => match buf.save(&path) {
                    Ok(()) => eprintln!("wrote {} ({w}x{h})", path.display()),
                    Err(e) => eprintln!("could not write {}: {e}", path.display()),
                },
                None => eprintln!("screenshot buffer had the wrong size"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

// ------------------------------------------------------------ helpers ------

fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn pill_button(ui: &mut Ui, p: &Palette, label: &str, enabled: bool) -> egui::Response {
    let galley_w = label.len() as f32 * 7.6 + 30.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(galley_w, 30.0), Sense::click());
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("h"), resp.hovered() && enabled, 0.12);
    let press =
        ui.ctx()
            .animate_bool_with_time(resp.id.with("p"), resp.is_pointer_button_down_on(), 0.07);

    let rect = rect.shrink(press * 1.5);
    let fill = if enabled {
        lerp_color(p.accent, p.accent_hi, t)
    } else {
        p.surface_hi
    };
    ui.painter().rect_filled(rect, radius::PILL, fill);
    if enabled && t > 0.01 {
        anim::glow_rect(ui.painter(), rect, radius::PILL, p.accent, t);
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.0),
        if enabled {
            Color32::from_rgb(20, 14, 8)
        } else {
            p.text_faint
        },
    );
    resp
}

fn ghost_button(ui: &mut Ui, p: &Palette, label: &str) -> egui::Response {
    let w = label.len() as f32 * 7.2 + 22.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 30.0), Sense::click());
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.12);
    ui.painter().rect_stroke(
        rect,
        radius::PILL,
        Stroke::new(1.0, lerp_color(p.outline, p.accent, t)),
        StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(12.5),
        lerp_color(p.text_dim, p.text, t),
    );
    resp
}

fn chip(ui: &mut Ui, p: &Palette, label: &str, active: bool) -> egui::Response {
    let w = label.len() as f32 * 6.9 + 20.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 24.0), Sense::click());
    let hover = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.12);
    let act = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("a"), active, 0.16);

    ui.painter().rect_filled(
        rect,
        radius::PILL,
        lerp_color(
            lerp_color(p.surface, p.surface_hi, hover),
            p.accent.gamma_multiply(0.30),
            act,
        ),
    );
    ui.painter().rect_stroke(
        rect,
        radius::PILL,
        Stroke::new(1.0, lerp_color(p.outline, p.accent, act.max(hover * 0.5))),
        StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(11.5),
        lerp_color(p.text_dim, p.text, act.max(hover)),
    );
    resp
}

fn settings_group(ui: &mut Ui, p: &Palette, title: &str, add: impl FnOnce(&mut Ui)) {
    ui.label(RichText::new(title).size(15.0).color(p.accent));
    ui.add_space(space::XS);
    theme::card(p, 0.0).show(ui, |ui| {
        ui.set_width(ui.available_width() - space::MD * 2.0);
        add(ui);
    });
    ui.add_space(space::MD);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_is_unicode_safe() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("héllo wörld", 6), "héllo…");
    }

    #[test]
    fn every_view_has_a_label_and_an_icon() {
        // Icons are vector-drawn rather than glyphs, so the guarantee here is
        // that the nav match in `draw_nav` stays exhaustive over View::ALL.
        for v in View::ALL {
            assert!(!v.label().is_empty(), "{v:?} has no label");
        }
        assert_eq!(View::ALL.len(), 4);
    }
}
