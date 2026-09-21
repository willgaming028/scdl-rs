//! Interactive terminal UI.
//!
//! The loop selects over three sources — terminal input, pipeline events, and an
//! animation tick — so downloads keep streaming while the user browses, and the
//! spinner keeps turning while the network is quiet.

pub mod app;
pub mod ui;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as TermEvent, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use ratatui::prelude::*;
use tokio::sync::mpsc;

use scdl_core::archive::Archive;
use scdl_core::client::Client;
use scdl_core::pipeline::{download_tracks, DownloadOptions, Event as PipeEvent};
use scdl_core::resolve::{resolve, ResolveOptions, Target};

use app::{App, Mode, Pane};

/// Restores the terminal on drop, so a panic or an early return cannot leave the
/// user staring at a raw-mode shell with no echo.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        Ok(Terminal::new(CrosstermBackend::new(stdout))?)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
    }
}

/// Messages the UI sends itself from background tasks.
enum UiMessage {
    Resolved {
        label: String,
        tracks: Vec<scdl_core::model::Track>,
    },
    ResolveFailed(String),
    Pipeline(PipeEvent),
}

pub async fn run(
    client: Client,
    output_dir: PathBuf,
    opts: DownloadOptions,
    archive: Option<Arc<tokio::sync::Mutex<Archive>>>,
    archive_path: Option<PathBuf>,
    initial: Option<Target>,
) -> Result<()> {
    let _guard = TerminalGuard;
    let mut terminal = TerminalGuard::enter()?;

    let mut app = App::new(output_dir.clone());
    app.info(format!("downloading into {}", output_dir.display()));
    if archive_path.is_some() {
        app.info("download archive active — already-downloaded tracks will be skipped");
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<UiMessage>();

    if let Some(target) = initial {
        spawn_resolve(&client, target, ResolveOptions::default(), tx.clone());
        app.mode = Mode::Loading("resolving".to_string());
    }

    let mut term_events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        terminal.draw(|f| ui::draw(f, &mut app))?;

        tokio::select! {
            _ = ticker.tick() => {
                app.tick = app.tick.wrapping_add(1);
            }

            Some(msg) = rx.recv() => {
                match msg {
                    UiMessage::Resolved { label, tracks } => {
                        app.mode = Mode::Browsing;
                        if tracks.is_empty() {
                            app.warn(format!("{label}: nothing to download"));
                            app.status = "No tracks found".to_string();
                        } else {
                            app.info(format!("{label}: {} track(s)", tracks.len()));
                            app.status = format!(
                                "{} tracks — Space to toggle, Enter to download",
                                tracks.len()
                            );
                            app.set_results(label, tracks);
                        }
                    }
                    UiMessage::ResolveFailed(e) => {
                        app.mode = Mode::Browsing;
                        app.error(e.clone());
                        app.status = "Resolve failed".to_string();
                    }
                    UiMessage::Pipeline(ev) => {
                        let done = matches!(ev, PipeEvent::Done { .. });
                        app.apply_event(ev);
                        if done {
                            if let (Some(arch), Some(path)) = (&archive, &archive_path) {
                                match arch.lock().await.save(path) {
                                    Ok(()) => app.info(format!("archive written to {}", path.display())),
                                    Err(e) => app.error(format!("could not write archive: {e}")),
                                }
                            }
                        }
                    }
                }
            }

            Some(Ok(ev)) = term_events.next() => {
                if let TermEvent::Key(key) = ev {
                    if key.kind == KeyEventKind::Press
                        && handle_key(key, &mut app, &client, &opts, &archive, &tx)
                    {
                        break;
                    }
                }
            }
        }

        if app.mode == Mode::Quitting {
            break;
        }
    }

    Ok(())
}

/// Returns true when the app should exit.
fn handle_key(
    key: KeyEvent,
    app: &mut App,
    client: &Client,
    opts: &DownloadOptions,
    archive: &Option<Arc<tokio::sync::Mutex<Archive>>>,
    tx: &mpsc::UnboundedSender<UiMessage>,
) -> bool {
    // Ctrl-C always quits, whatever the mode.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return true;
    }

    if app.mode == Mode::Help {
        app.mode = Mode::Browsing;
        return false;
    }

    if app.mode == Mode::Editing {
        match key.code {
            KeyCode::Enter => {
                let query = app.input.trim().to_string();
                if !query.is_empty() {
                    let target = classify_input(&query);
                    app.mode = Mode::Loading(match &target {
                        Target::Search(_) => "searching".to_string(),
                        _ => "resolving".to_string(),
                    });
                    app.info(format!("resolving {query}"));
                    spawn_resolve(client, target, ResolveOptions::default(), tx.clone());
                }
            }
            KeyCode::Esc => {
                app.mode = Mode::Browsing;
                app.pane = Pane::Results;
            }
            KeyCode::Tab => app.next_pane(),
            KeyCode::Backspace => app.backspace(),
            KeyCode::Left => app.move_input_cursor(-1),
            KeyCode::Right => app.move_input_cursor(1),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.clear_input()
            }
            KeyCode::Char(c) => app.insert_char(c),
            _ => {}
        }
        return false;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::Char('?') => app.mode = Mode::Help,
        KeyCode::Char('/') => {
            app.pane = Pane::Search;
            app.mode = Mode::Editing;
        }
        KeyCode::Tab => app.next_pane(),
        KeyCode::Char('j') | KeyCode::Down => app.move_cursor(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_cursor(-1),
        KeyCode::Char('g') | KeyCode::Home => app.move_cursor(isize::MIN / 2),
        KeyCode::Char('G') | KeyCode::End => app.move_cursor(isize::MAX / 2),
        KeyCode::Char(' ') => {
            if app.pane == Pane::Results {
                app.toggle_selection();
                app.move_cursor(1);
            }
        }
        KeyCode::Char('a') => app.select_all(true),
        KeyCode::Char('n') => app.select_all(false),
        KeyCode::Enter => {
            let tracks = app.selected_tracks();
            if tracks.is_empty() {
                app.warn("nothing selected");
            } else {
                app.info(format!("queued {} track(s)", tracks.len()));
                app.status = format!("Downloading {} track(s)…", tracks.len());
                app.pane = Pane::Queue;
                spawn_download(client, tracks, opts.clone(), archive.clone(), tx.clone());
            }
        }
        _ => {}
    }
    false
}

/// A URL if it looks like one, otherwise a search query.
fn classify_input(s: &str) -> Target {
    let looks_like_url = s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("soundcloud.com")
        || s.starts_with("www.soundcloud.com")
        || s.starts_with("m.soundcloud.com")
        || s.starts_with("on.soundcloud.com");

    if looks_like_url {
        Target::Url(s.to_string())
    } else if s == "me" {
        Target::Me
    } else {
        Target::Search(s.to_string())
    }
}

fn spawn_resolve(
    client: &Client,
    target: Target,
    opts: ResolveOptions,
    tx: mpsc::UnboundedSender<UiMessage>,
) {
    let client = client.clone();
    tokio::spawn(async move {
        let msg = match resolve(&client, &target, &opts).await {
            Ok(r) => UiMessage::Resolved {
                label: r.label,
                tracks: r.tracks,
            },
            Err(e) => UiMessage::ResolveFailed(e.to_string()),
        };
        let _ = tx.send(msg);
    });
}

fn spawn_download(
    client: &Client,
    tracks: Vec<scdl_core::model::Track>,
    opts: DownloadOptions,
    archive: Option<Arc<tokio::sync::Mutex<Archive>>>,
    tx: mpsc::UnboundedSender<UiMessage>,
) {
    let client = client.clone();
    tokio::spawn(async move {
        let (ptx, mut prx) = mpsc::unbounded_channel::<PipeEvent>();
        let forward = tokio::spawn(async move {
            while let Some(ev) = prx.recv().await {
                if tx.send(UiMessage::Pipeline(ev)).is_err() {
                    break;
                }
            }
        });
        download_tracks(&client, tracks, &opts, archive, ptx).await;
        let _ = forward.await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_distinguished_from_search_queries() {
        assert!(matches!(
            classify_input("https://soundcloud.com/a/b"),
            Target::Url(_)
        ));
        assert!(matches!(
            classify_input("soundcloud.com/a/b"),
            Target::Url(_)
        ));
        assert!(matches!(classify_input("me"), Target::Me));
        assert!(matches!(classify_input("aphex twin"), Target::Search(_)));
        // A bare word is a search, not a half-formed URL.
        assert!(matches!(classify_input("boards"), Target::Search(_)));
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn typing_then_escape_switches_modes() {
        let mut app = App::new(PathBuf::from("/tmp"));
        let client = Client::new(Default::default()).unwrap();
        let opts = DownloadOptions::default();
        let (tx, _rx) = mpsc::unbounded_channel();

        assert_eq!(app.mode, Mode::Editing);
        for c in "test".chars() {
            handle_key(
                press(KeyCode::Char(c)),
                &mut app,
                &client,
                &opts,
                &None,
                &tx,
            );
        }
        assert_eq!(app.input, "test");

        handle_key(press(KeyCode::Esc), &mut app, &client, &opts, &None, &tx);
        assert_eq!(app.mode, Mode::Browsing);

        // 'q' only quits outside the search box.
        let quit = handle_key(
            press(KeyCode::Char('q')),
            &mut app,
            &client,
            &opts,
            &None,
            &tx,
        );
        assert!(quit);
    }

    #[test]
    fn q_does_not_quit_while_typing() {
        let mut app = App::new(PathBuf::from("/tmp"));
        let client = Client::new(Default::default()).unwrap();
        let opts = DownloadOptions::default();
        let (tx, _rx) = mpsc::unbounded_channel();

        let quit = handle_key(
            press(KeyCode::Char('q')),
            &mut app,
            &client,
            &opts,
            &None,
            &tx,
        );
        assert!(!quit, "typing 'q' in the search box must not quit");
        assert_eq!(app.input, "q");
    }

    #[test]
    fn ctrl_c_always_quits() {
        let mut app = App::new(PathBuf::from("/tmp"));
        let client = Client::new(Default::default()).unwrap();
        let opts = DownloadOptions::default();
        let (tx, _rx) = mpsc::unbounded_channel();
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(handle_key(key, &mut app, &client, &opts, &None, &tx));
    }

    #[test]
    fn help_is_dismissed_by_any_key() {
        let mut app = App::new(PathBuf::from("/tmp"));
        let client = Client::new(Default::default()).unwrap();
        let opts = DownloadOptions::default();
        let (tx, _rx) = mpsc::unbounded_channel();
        app.mode = Mode::Help;
        handle_key(
            press(KeyCode::Char('x')),
            &mut app,
            &client,
            &opts,
            &None,
            &tx,
        );
        assert_eq!(app.mode, Mode::Browsing);
    }
}
