//! Application state.
//!
//! Mirrors the TUI's state model so both front-ends behave alike, but keeps its
//! own types: the GUI needs per-item animation state (springs, entry timers,
//! texture handles) that would be meaningless in a terminal.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use egui::TextureHandle;
use scdl_core::model::Track;
use scdl_core::pipeline::Event;
use scdl_core::resolve::Selector;

use crate::anim::Spring;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Browse,
    Queue,
    Library,
    Settings,
}

impl View {
    pub const ALL: [View; 4] = [View::Browse, View::Queue, View::Library, View::Settings];

    pub fn label(self) -> &'static str {
        match self {
            View::Browse => "Browse",
            View::Queue => "Queue",
            View::Library => "Library",
            View::Settings => "Settings",
        }
    }
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

    pub fn is_active(self) -> bool {
        matches!(
            self,
            ItemState::Resolving | ItemState::Downloading | ItemState::Tagging
        )
    }
}

/// One row of the download queue, with its animation state.
#[derive(Debug, Clone)]
pub struct QueueItem {
    pub track_id: i64,
    pub title: String,
    pub artist: String,
    pub state: ItemState,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub path: Option<PathBuf>,
    pub format: Option<String>,
    pub message: Option<String>,

    /// Smoothed 0..1 for the progress ring, so a jumpy byte count reads as
    /// continuous motion.
    pub progress: Spring,
    /// Smoothed bytes/sec.
    pub speed: Spring,
    /// When this item entered the queue, for the staggered slide-in.
    pub born: Instant,
    /// Set when the item reaches a terminal state, for the exit flourish.
    pub finished_at: Option<Instant>,

    last_bytes: u64,
    last_tick: Option<Instant>,
}

impl QueueItem {
    fn new(track_id: i64, title: String, artist: String) -> Self {
        Self {
            track_id,
            title,
            artist,
            state: ItemState::Queued,
            downloaded: 0,
            total: None,
            path: None,
            format: None,
            message: None,
            progress: Spring::new(0.0),
            speed: Spring::new(0.0),
            born: Instant::now(),
            finished_at: None,
            last_bytes: 0,
            last_tick: None,
        }
    }

    /// Target value for the progress ring, or `None` when indeterminate.
    pub fn target_fraction(&self) -> Option<f32> {
        match self.state {
            ItemState::Done => Some(1.0),
            ItemState::Skipped | ItemState::Failed => None,
            _ => self
                .total
                .filter(|t| *t > 0)
                .map(|t| (self.downloaded as f64 / t as f64).clamp(0.0, 1.0) as f32),
        }
    }

    pub fn eta_secs(&self) -> Option<f32> {
        let total = self.total?;
        let speed = self.speed.value;
        if speed < 1.0 {
            return None;
        }
        Some(total.saturating_sub(self.downloaded) as f32 / speed)
    }

    fn record(&mut self, downloaded: u64, total: Option<u64>) {
        let now = Instant::now();
        self.downloaded = downloaded;
        if total.is_some() {
            self.total = total;
        }
        if let Some(last) = self.last_tick {
            let dt = now.duration_since(last).as_secs_f32();
            if dt >= 0.2 {
                let delta = downloaded.saturating_sub(self.last_bytes) as f32;
                // Fed into a spring by the UI each frame; this is the raw target.
                self.speed.step(delta / dt, dt.min(0.5));
                self.last_bytes = downloaded;
                self.last_tick = Some(now);
            }
        } else {
            self.last_tick = Some(now);
            self.last_bytes = downloaded;
        }
    }
}

/// A search/resolve result awaiting selection.
#[derive(Debug, Clone)]
pub struct ResultItem {
    pub track: Track,
    pub selected: bool,
    /// Index within the result set, for staggering the entry animation.
    pub ordinal: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub born: Instant,
    pub lifetime: f32,
}

impl Toast {
    pub fn age(&self) -> f32 {
        self.born.elapsed().as_secs_f32()
    }

    pub fn expired(&self) -> bool {
        self.age() > self.lifetime
    }

    /// 0..1 remaining, for the dismissal hairline.
    pub fn remaining(&self) -> f32 {
        (1.0 - self.age() / self.lifetime).clamp(0.0, 1.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Success,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub text: String,
    pub level: LogLevel,
}

/// A finished download, shown in the Library view.
#[derive(Debug, Clone)]
pub struct LibraryEntry {
    pub track_id: i64,
    pub title: String,
    pub artist: String,
    pub path: PathBuf,
    pub bytes: u64,
}

pub struct AppState {
    pub view: View,
    pub query: String,
    pub selector: Selector,

    pub results: Vec<ResultItem>,
    pub results_label: String,
    pub resolving: bool,

    pub queue: Vec<QueueItem>,
    queue_index: HashMap<usize, usize>,

    pub library: Vec<LibraryEntry>,
    pub logs: Vec<LogLine>,
    pub toasts: Vec<Toast>,

    /// Cover-art textures, keyed by track id. `None` means a fetch is in flight
    /// so it is not requested twice.
    pub art: HashMap<i64, Option<TextureHandle>>,

    pub completed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub total_bytes: u64,

    /// Rolling download-speed history for the sparkline, in bytes/sec.
    pub speed_history: Vec<f32>,
    last_history_push: Option<Instant>,

    pub output_dir: PathBuf,
    pub name_format: String,
    pub playlist_name_format: String,
    pub auth_token: String,
    pub concurrency: usize,
    pub only_mp3: bool,
    pub allow_opus: bool,
    pub use_archive: bool,

    pub mode: crate::theme::Mode,
    pub animations_on: bool,
}

impl AppState {
    pub fn new(output_dir: PathBuf) -> Self {
        Self {
            view: View::Browse,
            query: String::new(),
            selector: Selector::Tracks,
            results: Vec::new(),
            results_label: String::new(),
            resolving: false,
            queue: Vec::new(),
            queue_index: HashMap::new(),
            library: Vec::new(),
            logs: Vec::new(),
            toasts: Vec::new(),
            art: HashMap::new(),
            completed: 0,
            skipped: 0,
            failed: 0,
            total_bytes: 0,
            speed_history: vec![0.0; 120],
            last_history_push: None,
            output_dir,
            name_format: scdl_core::naming::DEFAULT_NAME_FORMAT.to_string(),
            playlist_name_format: scdl_core::naming::DEFAULT_PLAYLIST_NAME_FORMAT.to_string(),
            auth_token: String::new(),
            concurrency: 3,
            only_mp3: false,
            allow_opus: false,
            use_archive: true,
            mode: crate::theme::Mode::Dark,
            animations_on: true,
        }
    }

    // ---- logging & toasts ----

    pub fn log(&mut self, level: LogLevel, text: impl Into<String>) {
        self.logs.push(LogLine {
            text: text.into(),
            level,
        });
        if self.logs.len() > 400 {
            self.logs.drain(..80);
        }
    }

    pub fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        let text = text.into();
        self.log(
            match kind {
                ToastKind::Info => LogLevel::Info,
                ToastKind::Success => LogLevel::Success,
                ToastKind::Warn => LogLevel::Warn,
                ToastKind::Error => LogLevel::Error,
            },
            text.clone(),
        );
        self.toasts.push(Toast {
            text,
            kind,
            born: Instant::now(),
            lifetime: if kind == ToastKind::Error { 7.0 } else { 4.0 },
        });
        // Keep the stack from growing without bound during a big run.
        if self.toasts.len() > 5 {
            self.toasts.remove(0);
        }
    }

    pub fn prune_toasts(&mut self) {
        self.toasts.retain(|t| !t.expired());
    }

    // ---- results ----

    pub fn set_results(&mut self, label: String, tracks: Vec<Track>) {
        self.results_label = label;
        self.results = tracks
            .into_iter()
            .enumerate()
            .map(|(ordinal, track)| ResultItem {
                track,
                selected: true,
                ordinal,
            })
            .collect();
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

    pub fn select_all(&mut self, on: bool) {
        for r in &mut self.results {
            r.selected = on;
        }
    }

    // ---- queue ----

    pub fn active_count(&self) -> usize {
        self.queue.iter().filter(|i| !i.state.is_terminal()).count()
    }

    /// Total in-flight throughput, for the header readout and sparkline.
    pub fn aggregate_speed(&self) -> f32 {
        self.queue
            .iter()
            .filter(|i| i.state == ItemState::Downloading)
            .map(|i| i.speed.value)
            .sum()
    }

    pub fn overall_fraction(&self) -> f32 {
        if self.queue.is_empty() {
            return 0.0;
        }
        let sum: f32 = self
            .queue
            .iter()
            .map(|i| {
                if i.state.is_terminal() {
                    1.0
                } else {
                    i.target_fraction().unwrap_or(0.0)
                }
            })
            .sum();
        sum / self.queue.len() as f32
    }

    /// The item the hero panel should feature: the furthest-along active one.
    pub fn hero_item(&self) -> Option<&QueueItem> {
        self.queue
            .iter()
            .filter(|i| i.state.is_active())
            .max_by(|a, b| {
                a.target_fraction()
                    .unwrap_or(0.0)
                    .partial_cmp(&b.target_fraction().unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .or_else(|| self.queue.last())
    }

    /// Push the current aggregate speed into the sparkline history, at most
    /// ~10 times a second so the graph scrolls at a readable rate.
    pub fn tick_history(&mut self) {
        let now = Instant::now();
        let due = self
            .last_history_push
            .is_none_or(|t| now.duration_since(t).as_secs_f32() >= 0.1);
        if !due {
            return;
        }
        self.last_history_push = Some(now);
        self.speed_history.remove(0);
        self.speed_history.push(self.aggregate_speed());
    }

    /// Fold a pipeline event into the state.
    pub fn apply_event(&mut self, ev: Event) {
        match ev {
            Event::Queued {
                index,
                id,
                title,
                artist,
            } => {
                let slot = self.queue.len();
                self.queue.push(QueueItem::new(id, title, artist));
                self.queue_index.insert(index, slot);
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
                }
            }
            Event::Progress {
                index,
                downloaded,
                total,
            } => {
                if let Some(i) = self.item_mut(index) {
                    i.record(downloaded, total);
                    if i.state != ItemState::Downloading {
                        i.state = ItemState::Downloading;
                    }
                }
            }
            Event::Tagging { index } => {
                if let Some(i) = self.item_mut(index) {
                    i.state = ItemState::Tagging;
                }
            }
            Event::Finished { index, path, bytes } => {
                let mut entry = None;
                if let Some(i) = self.item_mut(index) {
                    i.state = ItemState::Done;
                    i.downloaded = bytes;
                    i.total = Some(bytes);
                    i.path = Some(path.clone());
                    i.finished_at = Some(Instant::now());
                    entry = Some(LibraryEntry {
                        track_id: i.track_id,
                        title: i.title.clone(),
                        artist: i.artist.clone(),
                        path,
                        bytes,
                    });
                }
                self.completed += 1;
                self.total_bytes += bytes;
                if let Some(e) = entry {
                    let label = format!("{} — {}", e.artist, e.title);
                    self.library.push(e);
                    self.toast(ToastKind::Success, label);
                }
            }
            Event::Skipped { index, reason } => {
                let mut label = None;
                if let Some(i) = self.item_mut(index) {
                    i.state = ItemState::Skipped;
                    i.message = Some(reason.clone());
                    i.finished_at = Some(Instant::now());
                    label = Some(i.title.clone());
                }
                self.skipped += 1;
                if let Some(l) = label {
                    self.log(LogLevel::Info, format!("skipped {l}: {reason}"));
                }
            }
            Event::Failed { index, error } => {
                let mut label = None;
                if let Some(i) = self.item_mut(index) {
                    i.state = ItemState::Failed;
                    i.message = Some(error.clone());
                    i.finished_at = Some(Instant::now());
                    label = Some(i.title.clone());
                }
                self.failed += 1;
                let l = label.unwrap_or_default();
                self.toast(ToastKind::Error, format!("{l}: {error}"));
            }
            Event::Done {
                completed,
                skipped,
                failed,
            } => {
                self.toast(
                    if failed > 0 {
                        ToastKind::Warn
                    } else {
                        ToastKind::Success
                    },
                    format!(
                        "Finished — {completed} downloaded, {skipped} skipped, {failed} failed"
                    ),
                );
            }
        }
    }

    fn item_mut(&mut self, index: usize) -> Option<&mut QueueItem> {
        let slot = *self.queue_index.get(&index)?;
        self.queue.get_mut(slot)
    }

    /// True while anything needs continuous repainting.
    pub fn needs_animation(&self) -> bool {
        self.resolving
            || !self.toasts.is_empty()
            || self.queue.iter().any(|i| !i.state.is_terminal())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        AppState::new(PathBuf::from("/tmp"))
    }

    fn track(id: i64, title: &str) -> Track {
        let mut t: Track = serde_json::from_str(r#"{"id":0}"#).unwrap();
        t.id = id;
        t.title = Some(title.into());
        t
    }

    #[test]
    fn event_lifecycle_updates_the_item() {
        let mut s = state();
        s.apply_event(Event::Queued {
            index: 0,
            id: 7,
            title: "T".into(),
            artist: "A".into(),
        });
        assert_eq!(s.queue[0].state, ItemState::Queued);

        s.apply_event(Event::Started {
            index: 0,
            path: PathBuf::from("/tmp/a.m4a"),
            format: "hls_aac".into(),
        });
        assert_eq!(s.queue[0].state, ItemState::Downloading);

        s.apply_event(Event::Progress {
            index: 0,
            downloaded: 25,
            total: Some(100),
        });
        assert_eq!(s.queue[0].target_fraction(), Some(0.25));

        s.apply_event(Event::Finished {
            index: 0,
            path: PathBuf::from("/tmp/a.m4a"),
            bytes: 100,
        });
        assert_eq!(s.queue[0].state, ItemState::Done);
        assert_eq!(s.completed, 1);
        assert_eq!(s.total_bytes, 100);
        assert_eq!(
            s.library.len(),
            1,
            "finished track should enter the library"
        );
    }

    #[test]
    fn events_for_unknown_indices_are_ignored() {
        let mut s = state();
        s.apply_event(Event::Progress {
            index: 99,
            downloaded: 1,
            total: None,
        });
        assert!(s.queue.is_empty());
    }

    #[test]
    fn queue_lookup_uses_event_index_not_insertion_order() {
        let mut s = state();
        for idx in [4usize, 1, 9] {
            s.apply_event(Event::Queued {
                index: idx,
                id: idx as i64,
                title: format!("T{idx}"),
                artist: "A".into(),
            });
        }
        s.apply_event(Event::Failed {
            index: 1,
            error: "boom".into(),
        });
        let failed: Vec<&QueueItem> = s
            .queue
            .iter()
            .filter(|i| i.state == ItemState::Failed)
            .collect();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].title, "T1");
    }

    #[test]
    fn overall_fraction_counts_terminal_items_as_whole() {
        let mut s = state();
        for i in 0..4 {
            s.apply_event(Event::Queued {
                index: i,
                id: i as i64,
                title: "t".into(),
                artist: "a".into(),
            });
        }
        assert_eq!(s.overall_fraction(), 0.0);
        s.apply_event(Event::Finished {
            index: 0,
            path: PathBuf::from("/x"),
            bytes: 1,
        });
        s.apply_event(Event::Skipped {
            index: 1,
            reason: "r".into(),
        });
        assert!((s.overall_fraction() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn failures_raise_an_error_toast() {
        let mut s = state();
        s.apply_event(Event::Queued {
            index: 0,
            id: 1,
            title: "T".into(),
            artist: "A".into(),
        });
        s.apply_event(Event::Failed {
            index: 0,
            error: "DRM".into(),
        });
        assert_eq!(s.toasts.len(), 1);
        assert_eq!(s.toasts[0].kind, ToastKind::Error);
        assert!(s.toasts[0].text.contains("DRM"));
    }

    #[test]
    fn toast_stack_is_bounded() {
        let mut s = state();
        for i in 0..20 {
            s.toast(ToastKind::Info, format!("n{i}"));
        }
        assert!(s.toasts.len() <= 5);
        assert!(s.toasts.last().unwrap().text.contains("19"));
    }

    #[test]
    fn logs_are_bounded() {
        let mut s = state();
        for i in 0..600 {
            s.log(LogLevel::Info, format!("l{i}"));
        }
        assert!(s.logs.len() <= 400);
    }

    #[test]
    fn selection_defaults_to_all_and_toggles() {
        let mut s = state();
        s.set_results(
            "R".into(),
            vec![track(1, "a"), track(2, "b"), track(3, "c")],
        );
        assert_eq!(s.selected_count(), 3);
        s.results[1].selected = false;
        assert_eq!(s.selected_count(), 2);
        assert_eq!(s.selected_tracks().len(), 2);
        s.select_all(false);
        assert_eq!(s.selected_count(), 0);
    }

    #[test]
    fn results_carry_an_ordinal_for_staggering() {
        let mut s = state();
        s.set_results("R".into(), vec![track(1, "a"), track(2, "b")]);
        assert_eq!(s.results[0].ordinal, 0);
        assert_eq!(s.results[1].ordinal, 1);
    }

    #[test]
    fn needs_animation_tracks_in_flight_work() {
        let mut s = state();
        assert!(!s.needs_animation());
        s.apply_event(Event::Queued {
            index: 0,
            id: 1,
            title: "t".into(),
            artist: "a".into(),
        });
        assert!(s.needs_animation(), "a queued item should keep repainting");
        s.apply_event(Event::Finished {
            index: 0,
            path: PathBuf::from("/x"),
            bytes: 1,
        });
        s.toasts.clear();
        assert!(!s.needs_animation(), "idle state should stop repainting");
    }

    #[test]
    fn speed_history_is_fixed_length() {
        let mut s = state();
        let n = s.speed_history.len();
        for _ in 0..50 {
            s.last_history_push = None; // force
            s.tick_history();
        }
        assert_eq!(s.speed_history.len(), n);
    }

    #[test]
    fn eta_is_none_without_a_speed() {
        let mut s = state();
        s.apply_event(Event::Queued {
            index: 0,
            id: 1,
            title: "t".into(),
            artist: "a".into(),
        });
        s.apply_event(Event::Progress {
            index: 0,
            downloaded: 10,
            total: Some(100),
        });
        assert!(s.queue[0].eta_secs().is_none());
    }

    #[test]
    fn hero_prefers_the_furthest_along_active_item() {
        let mut s = state();
        for i in 0..3 {
            s.apply_event(Event::Queued {
                index: i,
                id: i as i64,
                title: format!("t{i}"),
                artist: "a".into(),
            });
            s.apply_event(Event::Progress {
                index: i,
                downloaded: (i as u64 + 1) * 10,
                total: Some(100),
            });
        }
        assert_eq!(s.hero_item().unwrap().title, "t2");
    }
}
