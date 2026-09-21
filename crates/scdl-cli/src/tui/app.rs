//! TUI application state.
//!
//! The UI owns all state and is a pure function of it; background workers only
//! ever send [`Event`]s down a channel. Nothing in here blocks, so the render
//! loop stays responsive while downloads run.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use scdl_core::model::Track;
use scdl_core::pipeline::Event;

/// Which pane has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Search,
    Results,
    Queue,
}

/// What the app is doing right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Browsing,
    /// Typing in the search box.
    Editing,
    /// A resolve or search request is in flight.
    Loading(String),
    Help,
    Quitting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
    Queued,
    Resolving,
    Downloading,
    Tagging,
    Done,
    Skipped,
    Failed,
}

impl ItemState {
    pub fn symbol(self) -> &'static str {
        match self {
            ItemState::Queued => "·",
            ItemState::Resolving => "◌",
            ItemState::Downloading => "▼",
            ItemState::Tagging => "◈",
            ItemState::Done => "✓",
            ItemState::Skipped => "–",
            ItemState::Failed => "✗",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ItemState::Queued => "queued",
            ItemState::Resolving => "resolving",
            ItemState::Downloading => "downloading",
            ItemState::Tagging => "tagging",
            ItemState::Done => "done",
            ItemState::Skipped => "skipped",
            ItemState::Failed => "failed",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            ItemState::Done | ItemState::Skipped | ItemState::Failed
        )
    }
}

/// One row in the download queue.
#[derive(Debug, Clone)]
pub struct QueueItem {
    pub title: String,
    pub artist: String,
    pub state: ItemState,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub path: Option<PathBuf>,
    pub format: Option<String>,
    pub message: Option<String>,
    started: Option<Instant>,
    last_bytes: u64,
    last_tick: Option<Instant>,
    speed_bps: f64,
}

impl QueueItem {
    fn new(title: String, artist: String) -> Self {
        Self {
            title,
            artist,
            state: ItemState::Queued,
            downloaded: 0,
            total: None,
            path: None,
            format: None,
            message: None,
            started: None,
            last_bytes: 0,
            last_tick: None,
            speed_bps: 0.0,
        }
    }

    pub fn fraction(&self) -> Option<f64> {
        match self.state {
            ItemState::Done => Some(1.0),
            _ => self
                .total
                .filter(|t| *t > 0)
                .map(|t| (self.downloaded as f64 / t as f64).clamp(0.0, 1.0)),
        }
    }

    pub fn speed(&self) -> Option<f64> {
        (self.speed_bps > 0.0 && self.state == ItemState::Downloading).then_some(self.speed_bps)
    }

    /// Estimated seconds remaining, when both a total and a speed are known.
    pub fn eta(&self) -> Option<Duration> {
        let total = self.total?;
        let speed = self.speed()?;
        let remaining = total.saturating_sub(self.downloaded) as f64;
        (speed > 1.0).then(|| Duration::from_secs_f64(remaining / speed))
    }

    fn record_progress(&mut self, downloaded: u64, total: Option<u64>, now: Instant) {
        self.downloaded = downloaded;
        if total.is_some() {
            self.total = total;
        }
        self.state = ItemState::Downloading;
        self.started.get_or_insert(now);

        // Exponentially smoothed rate, so the number does not jitter with each chunk.
        if let Some(last) = self.last_tick {
            let dt = now.duration_since(last).as_secs_f64();
            if dt >= 0.25 {
                let delta = downloaded.saturating_sub(self.last_bytes) as f64;
                let instant = delta / dt;
                self.speed_bps = if self.speed_bps == 0.0 {
                    instant
                } else {
                    0.7 * self.speed_bps + 0.3 * instant
                };
                self.last_bytes = downloaded;
                self.last_tick = Some(now);
            }
        } else {
            self.last_tick = Some(now);
            self.last_bytes = downloaded;
        }
    }
}

/// A track found by a search or resolve, awaiting selection.
#[derive(Debug, Clone)]
pub struct ResultItem {
    pub track: Track,
    pub selected: bool,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub text: String,
    pub level: LogLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
    Success,
}

pub struct App {
    pub mode: Mode,
    pub pane: Pane,
    pub input: String,
    pub cursor: usize,

    pub results: Vec<ResultItem>,
    pub results_cursor: usize,
    pub results_title: String,

    pub queue: Vec<QueueItem>,
    queue_by_index: HashMap<usize, usize>,
    pub queue_cursor: usize,

    pub logs: Vec<LogLine>,

    pub output_dir: PathBuf,
    pub status: String,
    /// Advances on each tick, for spinner animation.
    pub tick: u64,

    pub completed: usize,
    pub skipped: usize,
    pub failed: usize,
}

impl App {
    pub fn new(output_dir: PathBuf) -> Self {
        Self {
            mode: Mode::Editing,
            pane: Pane::Search,
            input: String::new(),
            cursor: 0,
            results: Vec::new(),
            results_cursor: 0,
            results_title: "Results".to_string(),
            queue: Vec::new(),
            queue_by_index: HashMap::new(),
            queue_cursor: 0,
            logs: Vec::new(),
            output_dir,
            status: "Paste a SoundCloud URL or type a search, then press Enter".to_string(),
            tick: 0,
            completed: 0,
            skipped: 0,
            failed: 0,
        }
    }

    pub fn log(&mut self, level: LogLevel, text: impl Into<String>) {
        self.logs.push(LogLine {
            text: text.into(),
            level,
        });
        // Keep memory bounded on long runs.
        if self.logs.len() > 500 {
            self.logs.drain(..100);
        }
    }

    pub fn info(&mut self, t: impl Into<String>) {
        self.log(LogLevel::Info, t)
    }
    pub fn warn(&mut self, t: impl Into<String>) {
        self.log(LogLevel::Warn, t)
    }
    pub fn error(&mut self, t: impl Into<String>) {
        self.log(LogLevel::Error, t)
    }
    pub fn success(&mut self, t: impl Into<String>) {
        self.log(LogLevel::Success, t)
    }

    pub fn set_results(&mut self, title: String, tracks: Vec<Track>) {
        self.results_title = title;
        self.results = tracks
            .into_iter()
            .map(|track| ResultItem {
                track,
                // Pre-selected: the common case is "download all of this".
                selected: true,
            })
            .collect();
        self.results_cursor = 0;
        if !self.results.is_empty() {
            self.pane = Pane::Results;
            self.mode = Mode::Browsing;
        }
    }

    pub fn selected_tracks(&self) -> Vec<Track> {
        self.results
            .iter()
            .filter(|r| r.selected)
            .map(|r| r.track.clone())
            .collect()
    }

    pub fn selected_count(&self) -> usize {
        self.results.iter().filter(|r| r.selected).count()
    }

    pub fn toggle_selection(&mut self) {
        if let Some(r) = self.results.get_mut(self.results_cursor) {
            r.selected = !r.selected;
        }
    }

    pub fn select_all(&mut self, selected: bool) {
        for r in &mut self.results {
            r.selected = selected;
        }
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let (len, cursor) = match self.pane {
            Pane::Results => (self.results.len(), &mut self.results_cursor),
            Pane::Queue => (self.queue.len(), &mut self.queue_cursor),
            Pane::Search => return,
        };
        if len == 0 {
            *cursor = 0;
            return;
        }
        let next = (*cursor as isize + delta).clamp(0, len as isize - 1);
        *cursor = next as usize;
    }

    pub fn next_pane(&mut self) {
        self.pane = match self.pane {
            Pane::Search => Pane::Results,
            Pane::Results => Pane::Queue,
            Pane::Queue => Pane::Search,
        };
        if self.pane == Pane::Search {
            self.mode = Mode::Editing;
        } else if self.mode == Mode::Editing {
            self.mode = Mode::Browsing;
        }
    }

    /// Fold a pipeline event into the UI state.
    pub fn apply_event(&mut self, ev: Event) {
        let now = Instant::now();
        match ev {
            Event::Queued {
                index,
                title,
                artist,
                ..
            } => {
                let slot = self.queue.len();
                self.queue.push(QueueItem::new(title, artist));
                self.queue_by_index.insert(index, slot);
            }
            Event::Resolving { index } => {
                if let Some(i) = self.item_mut(index) {
                    i.state = ItemState::Resolving;
                }
            }
            Event::Started {
                index,
                path,
                format,
            } => {
                if let Some(i) = self.item_mut(index) {
                    i.state = ItemState::Downloading;
                    i.path = Some(path);
                    i.format = Some(format);
                    i.started = Some(now);
                }
            }
            Event::Progress {
                index,
                downloaded,
                total,
            } => {
                if let Some(i) = self.item_mut(index) {
                    i.record_progress(downloaded, total, now);
                }
            }
            Event::Tagging { index } => {
                if let Some(i) = self.item_mut(index) {
                    i.state = ItemState::Tagging;
                }
            }
            Event::Finished { index, path, bytes } => {
                let mut label = None;
                if let Some(i) = self.item_mut(index) {
                    i.state = ItemState::Done;
                    i.downloaded = bytes;
                    i.total = Some(bytes);
                    i.path = Some(path.clone());
                    label = Some(format!("{} — {}", i.artist, i.title));
                }
                self.completed += 1;
                if let Some(l) = label {
                    self.success(format!("{l}  →  {}", path.display()));
                }
            }
            Event::Skipped { index, reason } => {
                let mut label = None;
                if let Some(i) = self.item_mut(index) {
                    i.state = ItemState::Skipped;
                    i.message = Some(reason.clone());
                    label = Some(i.title.clone());
                }
                self.skipped += 1;
                if let Some(l) = label {
                    self.info(format!("skipped {l}: {reason}"));
                }
            }
            Event::Failed { index, error } => {
                let mut label = None;
                if let Some(i) = self.item_mut(index) {
                    i.state = ItemState::Failed;
                    i.message = Some(error.clone());
                    label = Some(i.title.clone());
                }
                self.failed += 1;
                if let Some(l) = label {
                    self.error(format!("{l}: {error}"));
                }
            }
            Event::Done {
                completed,
                skipped,
                failed,
            } => {
                self.status = format!(
                    "Finished — {completed} downloaded, {skipped} skipped, {failed} failed"
                );
                self.mode = Mode::Browsing;
            }
        }
    }

    fn item_mut(&mut self, index: usize) -> Option<&mut QueueItem> {
        let slot = *self.queue_by_index.get(&index)?;
        self.queue.get_mut(slot)
    }

    pub fn active_downloads(&self) -> usize {
        self.queue.iter().filter(|i| !i.state.is_terminal()).count()
    }

    pub fn overall_fraction(&self) -> f64 {
        if self.queue.is_empty() {
            return 0.0;
        }
        let done = self
            .queue
            .iter()
            .map(|i| match i.state {
                ItemState::Done | ItemState::Skipped | ItemState::Failed => 1.0,
                _ => i.fraction().unwrap_or(0.0),
            })
            .sum::<f64>();
        done / self.queue.len() as f64
    }

    // --- Text input handling ---

    pub fn insert_char(&mut self, c: char) {
        let byte = self.byte_offset(self.cursor);
        self.input.insert(byte, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.byte_offset(self.cursor - 1);
        let end = self.byte_offset(self.cursor);
        self.input.replace_range(start..end, "");
        self.cursor -= 1;
    }

    pub fn move_input_cursor(&mut self, delta: isize) {
        let len = self.input.chars().count();
        self.cursor = (self.cursor as isize + delta).clamp(0, len as isize) as usize;
    }

    pub fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
    }

    /// Byte offset of the `n`th character, for correct handling of multi-byte input.
    fn byte_offset(&self, n: usize) -> usize {
        self.input
            .char_indices()
            .nth(n)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len())
    }
}

/// Human-readable byte count.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

pub fn human_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App::new(PathBuf::from("/tmp"))
    }

    fn track(id: i64, title: &str) -> Track {
        let mut t: Track = serde_json::from_str(r#"{"id":0}"#).unwrap();
        t.id = id;
        t.title = Some(title.to_string());
        t
    }

    #[test]
    fn events_drive_an_item_through_its_lifecycle() {
        let mut a = app();
        a.apply_event(Event::Queued {
            index: 0,
            id: 1,
            title: "T".into(),
            artist: "A".into(),
        });
        assert_eq!(a.queue[0].state, ItemState::Queued);

        a.apply_event(Event::Started {
            index: 0,
            path: PathBuf::from("/tmp/t.mp3"),
            format: "http_mp3".into(),
        });
        assert_eq!(a.queue[0].state, ItemState::Downloading);

        a.apply_event(Event::Progress {
            index: 0,
            downloaded: 50,
            total: Some(100),
        });
        assert_eq!(a.queue[0].fraction(), Some(0.5));

        a.apply_event(Event::Finished {
            index: 0,
            path: PathBuf::from("/tmp/t.mp3"),
            bytes: 100,
        });
        assert_eq!(a.queue[0].state, ItemState::Done);
        assert_eq!(a.completed, 1);
        assert_eq!(a.queue[0].fraction(), Some(1.0));
    }

    #[test]
    fn out_of_order_and_unknown_indices_do_not_panic() {
        let mut a = app();
        // A progress event for something never queued must be ignored.
        a.apply_event(Event::Progress {
            index: 42,
            downloaded: 1,
            total: None,
        });
        assert!(a.queue.is_empty());
    }

    #[test]
    fn queue_lookup_uses_event_index_not_position() {
        let mut a = app();
        // Queue events can arrive for indices that do not match insertion order.
        for idx in [5usize, 2, 9] {
            a.apply_event(Event::Queued {
                index: idx,
                id: idx as i64,
                title: format!("T{idx}"),
                artist: "A".into(),
            });
        }
        a.apply_event(Event::Finished {
            index: 2,
            path: PathBuf::from("/tmp/x"),
            bytes: 1,
        });
        let done: Vec<&QueueItem> = a
            .queue
            .iter()
            .filter(|i| i.state == ItemState::Done)
            .collect();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].title, "T2");
    }

    #[test]
    fn overall_progress_counts_terminal_states_as_complete() {
        let mut a = app();
        for i in 0..4 {
            a.apply_event(Event::Queued {
                index: i,
                id: i as i64,
                title: "t".into(),
                artist: "a".into(),
            });
        }
        assert_eq!(a.overall_fraction(), 0.0);
        a.apply_event(Event::Finished {
            index: 0,
            path: PathBuf::from("/x"),
            bytes: 1,
        });
        a.apply_event(Event::Skipped {
            index: 1,
            reason: "r".into(),
        });
        a.apply_event(Event::Failed {
            index: 2,
            error: "e".into(),
        });
        assert!((a.overall_fraction() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn selection_defaults_to_everything_and_toggles() {
        let mut a = app();
        a.set_results("R".into(), vec![track(1, "a"), track(2, "b")]);
        assert_eq!(a.selected_count(), 2);
        a.toggle_selection();
        assert_eq!(a.selected_count(), 1);
        a.select_all(false);
        assert_eq!(a.selected_count(), 0);
        assert!(a.selected_tracks().is_empty());
    }

    #[test]
    fn cursor_movement_is_clamped() {
        let mut a = app();
        a.set_results("R".into(), vec![track(1, "a"), track(2, "b")]);
        a.pane = Pane::Results;
        a.move_cursor(-5);
        assert_eq!(a.results_cursor, 0);
        a.move_cursor(99);
        assert_eq!(a.results_cursor, 1);
    }

    #[test]
    fn cursor_movement_on_empty_list_is_safe() {
        let mut a = app();
        a.pane = Pane::Results;
        a.move_cursor(3);
        assert_eq!(a.results_cursor, 0);
    }

    #[test]
    fn text_input_handles_multibyte_characters() {
        let mut a = app();
        for c in "aéb".chars() {
            a.insert_char(c);
        }
        assert_eq!(a.input, "aéb");
        a.backspace();
        assert_eq!(a.input, "aé");
        a.backspace();
        assert_eq!(a.input, "a", "must not split the multi-byte char");
    }

    #[test]
    fn input_cursor_insert_in_middle() {
        let mut a = app();
        for c in "ac".chars() {
            a.insert_char(c);
        }
        a.move_input_cursor(-1);
        a.insert_char('b');
        assert_eq!(a.input, "abc");
    }

    #[test]
    fn logs_are_bounded() {
        let mut a = app();
        for i in 0..700 {
            a.info(format!("line {i}"));
        }
        assert!(a.logs.len() <= 500);
        // Oldest dropped, newest kept.
        assert!(a.logs.last().unwrap().text.contains("699"));
    }

    #[test]
    fn human_bytes_is_readable() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(5_242_880), "5.0 MiB");
    }

    #[test]
    fn human_duration_is_readable() {
        assert_eq!(human_duration(Duration::from_secs(45)), "45s");
        assert_eq!(human_duration(Duration::from_secs(125)), "2m05s");
        assert_eq!(human_duration(Duration::from_secs(3725)), "1h02m");
    }

    #[test]
    fn pane_cycling_switches_edit_mode_appropriately() {
        let mut a = app();
        a.pane = Pane::Search;
        a.next_pane();
        assert_eq!(a.pane, Pane::Results);
        assert_eq!(a.mode, Mode::Browsing);
        a.next_pane();
        assert_eq!(a.pane, Pane::Queue);
        a.next_pane();
        assert_eq!(a.pane, Pane::Search);
        assert_eq!(a.mode, Mode::Editing);
    }
}
