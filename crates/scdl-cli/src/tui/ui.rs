//! Rendering. A pure function of [`App`] — no I/O, no mutation of app state
//! beyond the animation tick, so the layout can be unit-tested against a
//! `TestBackend`.

use ratatui::prelude::*;
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph,
};

use super::app::{human_bytes, human_duration, App, ItemState, LogLevel, Mode, Pane};

/// Palette chosen from the 16 ANSI colours plus a few indexed ones, so it
/// inherits the user's terminal theme and stays legible on light and dark
/// backgrounds alike. No hard-coded RGB.
mod theme {
    use ratatui::style::Color;

    pub const ACCENT: Color = Color::Rgb(255, 119, 0); // SoundCloud orange
    pub const ACCENT_DIM: Color = Color::Rgb(180, 84, 0);
    pub const OK: Color = Color::Green;
    pub const WARN: Color = Color::Yellow;
    pub const ERR: Color = Color::Red;
    pub const MUTED: Color = Color::DarkGray;
    pub const TEXT: Color = Color::Reset;
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();

    let chunks = Layout::vertical([
        Constraint::Length(3), // search bar
        Constraint::Min(8),    // results + queue
        Constraint::Length(6), // log
        Constraint::Length(1), // status bar
    ])
    .split(area);

    draw_search(f, chunks[0], app);

    let middle = Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(chunks[1]);
    draw_results(f, middle[0], app);
    draw_queue(f, middle[1], app);

    draw_log(f, chunks[2], app);
    draw_status(f, chunks[3], app);

    if app.mode == Mode::Help {
        draw_help(f, area, app);
    }
}

fn pane_block(title: &str, focused: bool) -> Block<'_> {
    let (border_style, title_style) = if focused {
        (
            Style::default().fg(theme::ACCENT),
            Style::default().fg(theme::ACCENT).bold(),
        )
    } else {
        (
            Style::default().fg(theme::MUTED),
            Style::default().fg(theme::MUTED),
        )
    };

    Block::default()
        .borders(Borders::ALL)
        .border_type(if focused {
            BorderType::Thick
        } else {
            BorderType::Rounded
        })
        .border_style(border_style)
        .title(Span::styled(format!(" {title} "), title_style))
}

fn draw_search(f: &mut Frame, area: Rect, app: &App) {
    let focused = app.pane == Pane::Search;
    let editing = app.mode == Mode::Editing;

    let content: Line = if app.input.is_empty() && !editing {
        Line::from(Span::styled(
            "paste a SoundCloud URL, or type to search…",
            Style::default().fg(theme::MUTED).italic(),
        ))
    } else {
        Line::from(vec![
            Span::styled("❯ ", Style::default().fg(theme::ACCENT).bold()),
            Span::styled(app.input.clone(), Style::default().fg(theme::TEXT)),
        ])
    };

    let title = match &app.mode {
        Mode::Loading(what) => {
            let spin = SPINNER[(app.tick as usize) % SPINNER.len()];
            format!("{spin} {what}")
        }
        _ => "Search".to_string(),
    };

    f.render_widget(
        Paragraph::new(content).block(pane_block(&title, focused)),
        area,
    );

    if focused && editing {
        // +3 for the border and the "❯ " prompt.
        let x = area.x + 3 + app.input.chars().take(app.cursor).count() as u16;
        f.set_cursor_position((x.min(area.right().saturating_sub(2)), area.y + 1));
    }
}

fn draw_results(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.pane == Pane::Results;
    let title = if app.results.is_empty() {
        app.results_title.clone()
    } else {
        format!(
            "{} — {}/{} selected",
            app.results_title,
            app.selected_count(),
            app.results.len()
        )
    };

    if app.results.is_empty() {
        let hint = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  Nothing loaded yet.",
                Style::default().fg(theme::MUTED),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Tab to the search box, paste a URL,",
                Style::default().fg(theme::MUTED),
            )),
            Line::from(Span::styled(
                "  and press Enter.",
                Style::default().fg(theme::MUTED),
            )),
        ])
        .block(pane_block(&title, focused));
        f.render_widget(hint, area);
        return;
    }

    let width = area.width.saturating_sub(8) as usize;
    let items: Vec<ListItem> = app
        .results
        .iter()
        .map(|r| {
            let mark = if r.selected { "◉" } else { "○" };
            let mark_style = if r.selected {
                Style::default().fg(theme::ACCENT)
            } else {
                Style::default().fg(theme::MUTED)
            };
            let dur = r
                .track
                .duration_secs()
                .map(|s| format!(" {}:{:02}", (s as u64) / 60, (s as u64) % 60))
                .unwrap_or_default();

            let title = truncate(r.track.title_or_untitled(), width.saturating_sub(dur.len()));
            ListItem::new(Line::from(vec![
                Span::styled(format!("{mark} "), mark_style),
                Span::styled(title, Style::default().fg(theme::TEXT)),
                Span::styled(dur, Style::default().fg(theme::MUTED)),
                Span::raw("  "),
                Span::styled(
                    truncate(r.track.artist(), 18),
                    Style::default().fg(theme::MUTED).italic(),
                ),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.results_cursor.min(app.results.len() - 1)));

    f.render_stateful_widget(
        List::new(items)
            .block(pane_block(&title, focused))
            .highlight_style(
                Style::default()
                    .bg(if focused {
                        theme::ACCENT_DIM
                    } else {
                        theme::MUTED
                    })
                    .fg(Color::White)
                    .bold(),
            )
            .highlight_symbol("▌"),
        area,
        &mut state,
    );
}

fn draw_queue(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.pane == Pane::Queue;
    let title = if app.queue.is_empty() {
        "Queue".to_string()
    } else {
        format!(
            "Queue — {} active, {} done, {} skipped, {} failed",
            app.active_downloads(),
            app.completed,
            app.skipped,
            app.failed
        )
    };

    let block = pane_block(&title, focused);

    if app.queue.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Select tracks and press Enter to download.",
                    Style::default().fg(theme::MUTED),
                )),
            ])
            .block(block),
            area,
        );
        return;
    }

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Two rows per item: a label line and a progress line.
    let rows = (inner.height as usize / 2).max(1);
    let start = app.queue_cursor.saturating_sub(rows.saturating_sub(1));
    let visible: Vec<_> = app.queue.iter().skip(start).take(rows).collect();

    let constraints: Vec<Constraint> = visible.iter().map(|_| Constraint::Length(2)).collect();
    if constraints.is_empty() {
        return;
    }
    let slots = Layout::vertical(constraints).split(inner);

    for (slot, item) in slots.iter().zip(visible.iter()) {
        let lines = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(*slot);

        let state_style = match item.state {
            ItemState::Done => Style::default().fg(theme::OK),
            ItemState::Failed => Style::default().fg(theme::ERR),
            ItemState::Skipped => Style::default().fg(theme::MUTED),
            ItemState::Downloading | ItemState::Tagging => Style::default().fg(theme::ACCENT),
            _ => Style::default().fg(theme::MUTED),
        };

        let symbol = if matches!(item.state, ItemState::Resolving | ItemState::Tagging) {
            SPINNER[(app.tick as usize) % SPINNER.len()]
        } else {
            item.state.symbol()
        };

        let name_width = inner.width.saturating_sub(24) as usize;
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{symbol} "), state_style),
                Span::styled(
                    truncate(&item.title, name_width),
                    Style::default().fg(theme::TEXT),
                ),
                Span::raw("  "),
                Span::styled(
                    truncate(&item.artist, 16),
                    Style::default().fg(theme::MUTED).italic(),
                ),
            ])),
            lines[0],
        );

        // Detail line: a gauge while downloading, a message otherwise.
        match item.state {
            ItemState::Downloading => {
                let mut detail = human_bytes(item.downloaded);
                if let Some(t) = item.total {
                    detail = format!("{detail} / {}", human_bytes(t));
                }
                if let Some(s) = item.speed() {
                    detail = format!("{detail}  {}/s", human_bytes(s as u64));
                }
                if let Some(eta) = item.eta() {
                    detail = format!("{detail}  ETA {}", human_duration(eta));
                }

                match item.fraction() {
                    Some(frac) => {
                        f.render_widget(
                            Gauge::default()
                                .gauge_style(Style::default().fg(theme::ACCENT))
                                .ratio(frac)
                                .label(Span::styled(
                                    detail,
                                    Style::default().fg(Color::White).bold(),
                                )),
                            lines[1],
                        );
                    }
                    None => {
                        // SoundCloud's HLS gives no Content-Length, so there is
                        // no percentage to show. Compose the byte counter and an
                        // indeterminate marquee into ONE line — rendering them as
                        // two overlapping widgets makes them clobber each other.
                        let total_w = lines[1].width as usize;
                        let text = format!(" {detail} ");
                        let text_w = text.chars().count().min(total_w);
                        let bar_w = total_w.saturating_sub(text_w);

                        let mut spans = vec![Span::styled(
                            text.chars().take(text_w).collect::<String>(),
                            Style::default().fg(Color::White),
                        )];

                        if bar_w > 0 {
                            let pos = (app.tick as usize) % bar_w;
                            let mut bar: Vec<char> = vec!['░'; bar_w];
                            for i in 0..6usize.min(bar_w) {
                                bar[(pos + i) % bar_w] = '█';
                            }
                            spans.push(Span::styled(
                                bar.into_iter().collect::<String>(),
                                Style::default().fg(theme::ACCENT_DIM),
                            ));
                        }

                        f.render_widget(Paragraph::new(Line::from(spans)), lines[1]);
                    }
                }
            }
            _ => {
                let msg = item
                    .message
                    .clone()
                    .unwrap_or_else(|| item.state.label().to_string());
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!(
                            "   {}",
                            truncate(&msg, inner.width.saturating_sub(4) as usize)
                        ),
                        Style::default().fg(match item.state {
                            ItemState::Failed => theme::ERR,
                            ItemState::Done => theme::OK,
                            _ => theme::MUTED,
                        }),
                    ))),
                    lines[1],
                );
            }
        }
    }
}

fn draw_log(f: &mut Frame, area: Rect, app: &App) {
    let block = pane_block("Log", false);
    let inner = block.inner(area);
    let inner_h = inner.height as usize;
    let start = app.logs.len().saturating_sub(inner_h);

    // Truncate rather than wrap. A wrapped long path (and download paths are
    // long) consumes several rows, so N log entries can overflow an N-row pane,
    // spill past the block and scroll the whole terminal.
    let text_width = inner.width.saturating_sub(3) as usize;

    let lines: Vec<Line> = app.logs[start..]
        .iter()
        .map(|l| {
            let (marker, style) = match l.level {
                LogLevel::Info => ("·", Style::default().fg(theme::MUTED)),
                LogLevel::Warn => ("!", Style::default().fg(theme::WARN)),
                LogLevel::Error => ("✗", Style::default().fg(theme::ERR)),
                LogLevel::Success => ("✓", Style::default().fg(theme::OK)),
            };
            Line::from(vec![
                Span::styled(format!(" {marker} "), style),
                Span::styled(
                    truncate_middle(&l.text, text_width),
                    Style::default().fg(theme::TEXT),
                ),
            ])
        })
        .collect();

    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let overall = app.overall_fraction();
    let progress = if app.queue.is_empty() {
        String::new()
    } else {
        format!(" {:.0}% ", overall * 100.0)
    };

    let keys = match app.mode {
        Mode::Editing => "Enter resolve · Tab panes · Esc browse · ? help",
        _ => "j/k move · Space select · a all · Enter download · Tab panes · / search · ? help · q quit",
    };

    // Width actually consumed on the left before the status text begins.
    let badge_w = 6 + env!("CARGO_PKG_VERSION").len() + progress.chars().count() + 1;
    let keys_w = keys.chars().count() as u16;

    // Only show the key hints when there is room for them AND for a useful
    // amount of status text; otherwise the two overlap and both become unreadable.
    let show_keys = area.width as usize > badge_w + keys_w as usize + 12;
    let status_budget = if show_keys {
        (area.width as usize).saturating_sub(badge_w + keys_w as usize + 2)
    } else {
        (area.width as usize).saturating_sub(badge_w + 1)
    };

    let left = Line::from(vec![
        Span::styled(
            format!(" scdl {}", env!("CARGO_PKG_VERSION")),
            Style::default().fg(Color::Black).bg(theme::ACCENT).bold(),
        ),
        Span::styled(
            progress,
            Style::default().fg(Color::Black).bg(theme::ACCENT_DIM),
        ),
        Span::raw(" "),
        Span::styled(
            truncate(&app.status, status_budget),
            Style::default().fg(theme::TEXT),
        ),
    ]);

    f.render_widget(Paragraph::new(left), area);

    if show_keys {
        let right = Rect {
            x: area.right() - keys_w - 1,
            y: area.y,
            width: keys_w,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Span::styled(keys, Style::default().fg(theme::MUTED))),
            right,
        );
    }
}

fn draw_help(f: &mut Frame, area: Rect, app: &App) {
    let w = 62.min(area.width.saturating_sub(4));
    let h = 22.min(area.height.saturating_sub(4));
    let popup = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };

    let row = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(
                format!("  {k:<12}"),
                Style::default().fg(theme::ACCENT).bold(),
            ),
            Span::styled(d.to_string(), Style::default().fg(theme::TEXT)),
        ])
    };

    let body = vec![
        Line::from(""),
        row("/", "focus the search box"),
        row("Enter", "resolve a URL, or download the selection"),
        row("j / ↓", "move down"),
        row("k / ↑", "move up"),
        row("Space", "toggle the track under the cursor"),
        row("a", "select all"),
        row("n", "select none"),
        row("Tab", "cycle panes"),
        row("Esc", "leave the search box"),
        row("?", "show or hide this help"),
        row("q", "quit"),
        Line::from(""),
        Line::from(vec![
            Span::styled("  saving to  ", Style::default().fg(theme::MUTED)),
            Span::styled(
                truncate_middle(
                    &app.output_dir.display().to_string(),
                    w.saturating_sub(16) as usize,
                ),
                Style::default().fg(theme::TEXT),
            ),
        ]),
        Line::from(Span::styled(
            "  Downloads keep running while you browse.",
            Style::default().fg(theme::MUTED).italic(),
        )),
    ];

    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Double)
                .border_style(Style::default().fg(theme::ACCENT))
                .title(Span::styled(
                    " Keys ",
                    Style::default().fg(theme::ACCENT).bold(),
                )),
        ),
        popup,
    );
}

/// Elide the middle, keeping both ends. Better than a tail ellipsis for paths,
/// where the filename is the informative part.
fn truncate_middle(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max < 8 {
        return truncate(s, max);
    }
    let keep = max - 1;
    let head = keep / 2;
    let tail = keep - head;
    let chars: Vec<char> = s.chars().collect();
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[count - tail..]);
    out
}

/// Truncate to a display width, adding an ellipsis when it does not fit.
fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::App;
    use ratatui::backend::TestBackend;
    use scdl_core::pipeline::Event;
    use std::path::PathBuf;

    fn render_at(w: u16, h: u16, app: &mut App) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_empty_state_without_panicking() {
        let mut app = App::new(PathBuf::from("/tmp"));
        let out = render_at(100, 30, &mut app);
        assert!(out.contains("Search"));
        assert!(out.contains("Queue"));
    }

    #[test]
    fn renders_a_populated_queue() {
        let mut app = App::new(PathBuf::from("/tmp"));
        app.apply_event(Event::Queued {
            index: 0,
            id: 1,
            title: "Milky Way".into(),
            artist: "pandadub".into(),
        });
        app.apply_event(Event::Started {
            index: 0,
            path: PathBuf::from("/tmp/a.m4a"),
            format: "hls_aac".into(),
        });
        app.apply_event(Event::Progress {
            index: 0,
            downloaded: 500_000,
            total: Some(1_000_000),
        });
        let out = render_at(100, 30, &mut app);
        assert!(out.contains("Milky Way"), "queue item not rendered:\n{out}");
        assert!(out.contains("pandadub"));
    }

    #[test]
    fn survives_very_small_terminals() {
        // Users resize; a panic here takes the whole app down.
        let mut app = App::new(PathBuf::from("/tmp"));
        app.apply_event(Event::Queued {
            index: 0,
            id: 1,
            title: "A track with quite a long name".into(),
            artist: "Some Artist".into(),
        });
        for (w, h) in [(20u16, 10u16), (40, 12), (10, 8), (200, 60), (30, 20)] {
            let _ = render_at(w, h, &mut app);
        }
    }

    #[test]
    fn help_overlay_renders() {
        let mut app = App::new(PathBuf::from("/tmp"));
        app.mode = Mode::Help;
        let out = render_at(100, 30, &mut app);
        assert!(out.contains("Keys"), "help overlay missing:\n{out}");
    }

    #[test]
    fn truncate_handles_unicode_and_edges() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hell…");
        assert_eq!(truncate("héllo wörld", 6), "héllo…");
        assert_eq!(truncate("x", 0), "");
    }

    #[test]
    fn indeterminate_progress_renders_without_a_total() {
        // HLS downloads report no total; the marquee path must not divide by zero.
        let mut app = App::new(PathBuf::from("/tmp"));
        app.apply_event(Event::Queued {
            index: 0,
            id: 1,
            title: "t".into(),
            artist: "a".into(),
        });
        app.apply_event(Event::Progress {
            index: 0,
            downloaded: 12345,
            total: None,
        });
        let _ = render_at(80, 24, &mut app);
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use crate::tui::app::{App, LogLevel};
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;

    /// A long log line must never push content out of the Log pane. Before this
    /// was fixed, wrapped download paths overflowed the block and scrolled the
    /// whole terminal, which wiped the search bar off the screen.
    #[test]
    fn long_log_lines_never_overflow_their_pane() {
        let mut app = App::new(PathBuf::from("/tmp"));
        for i in 0..40 {
            app.log(
                LogLevel::Success,
                format!(
                    "pandadub — {i} - A Very Long Track Title Indeed  →  \
                     /home/user/very/deeply/nested/music/library/The Lost Ship/\
                     {i}. pandadub - {i} - A Very Long Track Title Indeed.m4a"
                ),
            );
        }

        let mut term = Terminal::new(TestBackend::new(120, 38)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();

        let row = |y: u16| -> String {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect()
        };

        // The search bar occupies rows 0..3 and must still be intact.
        assert!(
            row(0).contains("Search"),
            "search bar was pushed off screen:\n{}",
            row(0)
        );
        // The status bar is the last row and must still be the status bar.
        assert!(
            row(37).contains("scdl"),
            "status bar clobbered:\n{}",
            row(37)
        );
    }

    #[test]
    fn status_bar_text_and_key_hints_do_not_overlap() {
        let mut app = App::new(PathBuf::from("/tmp"));
        app.status = "Finished — 10 downloaded, 0 skipped, 0 failed".to_string();

        for width in [60u16, 80, 100, 120, 200] {
            let mut term = Terminal::new(TestBackend::new(width, 20)).unwrap();
            term.draw(|f| draw(f, &mut app)).unwrap();
            let buf = term.backend().buffer().clone();
            let last: String = (0..buf.area.width)
                .map(|x| buf[(x, 19)].symbol().to_string())
                .collect();

            // Whatever fits, the row must never contain a mangled overlap: the
            // status text should either be fully present or cleanly elided.
            assert!(last.contains("scdl"), "width {width}: no badge:\n{last}");
            assert_eq!(
                last.chars().count(),
                width as usize,
                "width {width}: row length wrong"
            );
        }
    }

    #[test]
    fn truncate_middle_keeps_both_ends() {
        let p = "/home/user/music/The Lost Ship/01. pandadub - Milky Way.m4a";
        let t = truncate_middle(p, 30);
        assert!(t.chars().count() <= 30, "got {} chars", t.chars().count());
        assert!(t.starts_with("/home"), "lost the head: {t}");
        assert!(t.ends_with(".m4a"), "lost the tail: {t}");
        assert!(t.contains('…'));
    }

    #[test]
    fn truncate_middle_passes_short_strings_through() {
        assert_eq!(truncate_middle("short", 30), "short");
    }
}
