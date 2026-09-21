//! Download archives and playlist sync.
//!
//! # Why this module is careful
//!
//! The Python scdl's `--sync` deletes every file recorded in the archive that it
//! did not see during the current run. If a run enumerates nothing — a 404, a
//! 5xx, an expired `client_id`, a playlist gone private — "saw nothing" is
//! indistinguishable from "everything was removed upstream", and it deletes the
//! user's entire library and truncates the archive. That is reproducible in a
//! sandbox: three files and a valid archive, pointed at a playlist that 404s,
//! leaves an empty directory and a zero-byte archive.
//!
//! This implementation keeps the feature but makes that outcome impossible:
//!
//! * [`SyncPlan`] is only ever built from a **successfully enumerated** playlist;
//!   a failed enumeration cannot produce a plan at all.
//! * An empty remote listing is treated as suspicious and refuses to delete
//!   unless the caller explicitly opts in ([`SyncOptions::allow_empty_remote`]).
//! * Deletions are capped by [`SyncOptions::max_delete_fraction`]; a plan that
//!   would remove most of the library needs confirmation.
//! * Nothing outside the download directory is ever deleted, whatever the
//!   archive says.
//! * The archive is written atomically (temp file + rename), so a crash mid-write
//!   cannot truncate it.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::naming::is_within;

/// One archive entry: an extractor-qualified id, and where the file landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    /// Always `"soundcloud"` for us, but kept explicit for yt-dlp compatibility.
    pub extractor: String,
    pub id: String,
    /// Absent for plain `--download-archive` entries, which record no path.
    pub path: Option<PathBuf>,
}

impl ArchiveEntry {
    pub fn key(&self) -> String {
        format!("{} {}", self.extractor, self.id)
    }
}

/// A parsed archive file.
///
/// Ordering is preserved on rewrite so a user's archive does not churn in
/// version control.
#[derive(Debug, Clone, Default)]
pub struct Archive {
    entries: BTreeMap<String, ArchiveEntry>,
    /// Bare ids seen in legacy archives, matched loosely on lookup.
    legacy_ids: HashSet<String>,
}

impl Archive {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read an archive, tolerating both the modern and legacy line formats.
    ///
    /// A missing file is an empty archive, not an error. A malformed line *is*
    /// an error: silently ignoring one would understate what has been downloaded,
    /// and for sync that means deleting a file we should have kept.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(e) => return Err(Error::io(path, e)),
        };

        let mut archive = Self::new();
        for (lineno, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let mut parts = line.splitn(3, ' ');
            let first = parts.next().unwrap_or_default();
            let second = parts.next();
            let third = parts.next();

            match (first, second, third) {
                // "soundcloud 12345 /path/to/file.mp3"
                (ie, Some(id), Some(p)) if !ie.is_empty() && !id.is_empty() => {
                    let entry = ArchiveEntry {
                        extractor: ie.to_string(),
                        id: id.to_string(),
                        path: Some(PathBuf::from(p)),
                    };
                    archive.entries.insert(entry.key(), entry);
                }
                // "soundcloud 12345"
                (ie, Some(id), None) if !ie.is_empty() && !id.is_empty() => {
                    let entry = ArchiveEntry {
                        extractor: ie.to_string(),
                        id: id.to_string(),
                        path: None,
                    };
                    archive.entries.insert(entry.key(), entry);
                }
                // Legacy scdl 1.x: a bare track id per line.
                (id, None, None) if id.chars().all(|c| c.is_ascii_digit()) && !id.is_empty() => {
                    archive.legacy_ids.insert(id.to_string());
                }
                _ => {
                    return Err(Error::MalformedArchive {
                        path: path.to_path_buf(),
                        line: lineno + 1,
                        reason: format!("could not parse {line:?}"),
                    })
                }
            }
        }

        Ok(archive)
    }

    pub fn contains(&self, id: i64) -> bool {
        let id = id.to_string();
        self.entries.contains_key(&format!("soundcloud {id}")) || self.legacy_ids.contains(&id)
    }

    pub fn insert(&mut self, id: i64, path: Option<PathBuf>) {
        let entry = ArchiveEntry {
            extractor: "soundcloud".to_string(),
            id: id.to_string(),
            path,
        };
        self.entries.insert(entry.key(), entry);
    }

    pub fn remove(&mut self, id: i64) {
        let id = id.to_string();
        self.entries.remove(&format!("soundcloud {id}"));
        self.legacy_ids.remove(&id);
    }

    pub fn entries(&self) -> impl Iterator<Item = &ArchiveEntry> {
        self.entries.values()
    }

    pub fn len(&self) -> usize {
        self.entries.len() + self.legacy_ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Serialize, writing paths only where we have them.
    pub fn to_string_repr(&self) -> String {
        let mut out = String::new();
        for e in self.entries.values() {
            match &e.path {
                Some(p) => {
                    out.push_str(&format!("{} {} {}\n", e.extractor, e.id, p.display()));
                }
                None => out.push_str(&format!("{} {}\n", e.extractor, e.id)),
            }
        }
        for id in &self.legacy_ids {
            out.push_str(&format!("{id}\n"));
        }
        out
    }

    /// Write atomically: a temp file in the same directory, then a rename.
    ///
    /// The Python version opens the archive with mode `"w"`, which truncates
    /// before writing — a crash at that moment loses the entire record of what
    /// has been downloaded. A rename is atomic on POSIX, so the archive is
    /// always either the old content or the new one.
    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
        if let Some(dir) = parent {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }

        let dir = parent.unwrap_or_else(|| Path::new("."));
        let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| Error::io(dir, e))?;
        tmp.write_all(self.to_string_repr().as_bytes())
            .map_err(|e| Error::io(path, e))?;
        tmp.as_file().sync_all().map_err(|e| Error::io(path, e))?;
        tmp.persist(path).map_err(|e| Error::io(path, e.error))?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SyncOptions {
    /// Proceed even when the remote listing came back empty. Off by default:
    /// an empty listing usually means a failure, not an emptied playlist.
    pub allow_empty_remote: bool,
    /// Refuse to delete more than this fraction of the archive in one run.
    /// `1.0` disables the cap.
    pub max_delete_fraction: f64,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            allow_empty_remote: false,
            // Removing over half a library in one sync is almost always a bug
            // upstream rather than the user's intent.
            max_delete_fraction: 0.5,
        }
    }
}

/// What a sync would do, computed but not yet executed.
#[derive(Debug, Clone, Default)]
pub struct SyncPlan {
    /// Track ids present remotely but not in the archive.
    pub to_download: Vec<i64>,
    /// Files to delete, already validated as inside the download directory.
    pub to_delete: Vec<PathBuf>,
    /// Archive entries to drop that had no usable path on disk.
    pub stale_ids: Vec<i64>,
    /// Paths the archive listed that were refused for being outside the
    /// download directory. Reported, never deleted.
    pub refused: Vec<PathBuf>,
}

impl SyncPlan {
    pub fn is_noop(&self) -> bool {
        self.to_download.is_empty() && self.to_delete.is_empty() && self.stale_ids.is_empty()
    }
}

/// Build a sync plan.
///
/// `remote_ids` **must** come from a successful enumeration. The signature takes
/// them by value rather than reading them from somewhere fallible precisely so
/// that a caller cannot pass "nothing, because the request failed".
pub fn plan_sync(
    archive: &Archive,
    remote_ids: &[i64],
    download_dir: &Path,
    opts: &SyncOptions,
) -> Result<SyncPlan> {
    if remote_ids.is_empty() && !opts.allow_empty_remote {
        return Err(Error::Config(
            "refusing to sync: the remote listing was empty, which usually means the \
             request failed rather than that every track was removed. Re-run with \
             --sync-allow-empty if the playlist really is empty."
                .to_string(),
        ));
    }

    let remote: HashSet<String> = remote_ids.iter().map(i64::to_string).collect();
    let mut plan = SyncPlan::default();

    for entry in archive.entries() {
        if remote.contains(&entry.id) {
            continue;
        }
        match &entry.path {
            Some(p) => {
                // The archive is user-editable and, for a shared archive, not
                // fully trusted. Never delete outside the download directory.
                if !is_within(download_dir, p) {
                    plan.refused.push(p.clone());
                    continue;
                }
                if p.exists() {
                    plan.to_delete.push(p.clone());
                } else if let Ok(id) = entry.id.parse() {
                    plan.stale_ids.push(id);
                }
            }
            None => {
                if let Ok(id) = entry.id.parse() {
                    plan.stale_ids.push(id);
                }
            }
        }
    }

    for id in remote_ids {
        if !archive.contains(*id) {
            plan.to_download.push(*id);
        }
    }

    let total = archive.len();
    if opts.max_delete_fraction < 1.0 && total > 0 {
        let fraction = plan.to_delete.len() as f64 / total as f64;
        if fraction > opts.max_delete_fraction {
            return Err(Error::Config(format!(
                "refusing to sync: this would delete {} of {} archived files ({:.0}% of the \
                 archive), which exceeds the safety limit. Re-run with --sync-force if that \
                 is really what you want.",
                plan.to_delete.len(),
                total,
                fraction * 100.0
            )));
        }
    }

    Ok(plan)
}

/// Execute a plan's deletions, returning the paths actually removed.
pub fn apply_deletions(plan: &SyncPlan, download_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    for p in &plan.to_delete {
        // Re-check at the point of deletion; the plan may have been built a
        // while ago and this is cheap insurance against a TOCTOU mistake.
        if !is_within(download_dir, p) {
            return Err(Error::UnsafePath(p.clone()));
        }
        match std::fs::remove_file(p) {
            Ok(()) => removed.push(p.clone()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io(p, e)),
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write(p: &Path, s: &str) {
        std::fs::write(p, s).expect("write");
    }

    #[test]
    fn missing_archive_is_empty_not_an_error() {
        let d = dir();
        let a = Archive::load(&d.path().join("nope.txt")).unwrap();
        assert!(a.is_empty());
    }

    #[test]
    fn parses_modern_and_legacy_formats() {
        let d = dir();
        let p = d.path().join("a.txt");
        write(
            &p,
            "soundcloud 111 /music/a.mp3\nsoundcloud 222\n333\n\n# comment\n",
        );
        let a = Archive::load(&p).unwrap();
        assert!(a.contains(111));
        assert!(a.contains(222));
        assert!(a.contains(333), "legacy bare id should be recognised");
        assert!(!a.contains(444));
    }

    #[test]
    fn paths_with_spaces_survive_round_trip() {
        let d = dir();
        let p = d.path().join("a.txt");
        write(&p, "soundcloud 111 /music/My Track - Live (2019).mp3\n");
        let a = Archive::load(&p).unwrap();
        let e = a.entries().next().unwrap();
        assert_eq!(
            e.path.as_deref(),
            Some(Path::new("/music/My Track - Live (2019).mp3"))
        );
    }

    #[test]
    fn malformed_line_is_an_error_not_silently_skipped() {
        let d = dir();
        let p = d.path().join("a.txt");
        write(&p, "soundcloud 111 /music/a.mp3\nnot-a-valid-line\n");
        match Archive::load(&p) {
            Err(Error::MalformedArchive { line, .. }) => assert_eq!(line, 2),
            other => panic!("expected MalformedArchive, got {other:?}"),
        }
    }

    #[test]
    fn save_is_atomic_and_round_trips() {
        let d = dir();
        let p = d.path().join("a.txt");
        let mut a = Archive::new();
        a.insert(111, Some(PathBuf::from("/music/a.mp3")));
        a.insert(222, None);
        a.save(&p).unwrap();

        let b = Archive::load(&p).unwrap();
        assert!(b.contains(111) && b.contains(222));
        // No temp files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name() != "a.txt")
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind");
    }

    // --- The safety properties that distinguish this from the Python version ---

    #[test]
    fn empty_remote_listing_refuses_to_delete_anything() {
        // This is the exact scenario that wipes a library in the Python version:
        // the playlist 404s, nothing is enumerated, everything gets deleted.
        let d = dir();
        let mut a = Archive::new();
        for (id, name) in [(1i64, "a.mp3"), (2, "b.mp3"), (3, "c.mp3")] {
            let f = d.path().join(name);
            write(&f, "audio");
            a.insert(id, Some(f));
        }

        let err = plan_sync(&a, &[], d.path(), &SyncOptions::default()).unwrap_err();
        assert!(err.to_string().contains("empty"), "unexpected error: {err}");
        // And crucially, the files are still there.
        for name in ["a.mp3", "b.mp3", "c.mp3"] {
            assert!(d.path().join(name).exists(), "{name} was deleted");
        }
    }

    #[test]
    fn empty_remote_is_allowed_when_explicitly_opted_in() {
        let d = dir();
        let mut a = Archive::new();
        let f = d.path().join("a.mp3");
        write(&f, "audio");
        a.insert(1, Some(f));

        let opts = SyncOptions {
            allow_empty_remote: true,
            max_delete_fraction: 1.0,
        };
        let plan = plan_sync(&a, &[], d.path(), &opts).unwrap();
        assert_eq!(plan.to_delete.len(), 1);
    }

    #[test]
    fn mass_deletion_is_capped() {
        let d = dir();
        let mut a = Archive::new();
        for i in 0..10i64 {
            let f = d.path().join(format!("{i}.mp3"));
            write(&f, "audio");
            a.insert(i, Some(f));
        }
        // Remote still has one track, so this is not the "empty" guard firing.
        let err = plan_sync(&a, &[0], d.path(), &SyncOptions::default()).unwrap_err();
        assert!(err.to_string().contains("safety limit"), "got: {err}");
    }

    #[test]
    fn normal_sync_deletes_only_what_left_the_playlist() {
        let d = dir();
        let mut a = Archive::new();
        for i in 0..4i64 {
            let f = d.path().join(format!("{i}.mp3"));
            write(&f, "audio");
            a.insert(i, Some(f));
        }
        // Track 3 was removed upstream; track 9 is new.
        let plan = plan_sync(&a, &[0, 1, 2, 9], d.path(), &SyncOptions::default()).unwrap();
        assert_eq!(plan.to_download, vec![9]);
        assert_eq!(plan.to_delete, vec![d.path().join("3.mp3")]);

        let removed = apply_deletions(&plan, d.path()).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(!d.path().join("3.mp3").exists());
        assert!(d.path().join("0.mp3").exists());
    }

    #[test]
    fn paths_outside_the_download_directory_are_refused_never_deleted() {
        let d = dir();
        let outside = dir();
        let victim = outside.path().join("important.mp3");
        write(&victim, "do not delete me");

        let mut a = Archive::new();
        // A poisoned or stale archive pointing somewhere it should not.
        a.insert(1, Some(victim.clone()));
        a.insert(2, Some(d.path().join("ok.mp3")));
        write(&d.path().join("ok.mp3"), "audio");

        let opts = SyncOptions {
            allow_empty_remote: false,
            max_delete_fraction: 1.0,
        };
        let plan = plan_sync(&a, &[2], d.path(), &opts).unwrap();

        assert!(plan.to_delete.is_empty(), "nothing should be deleted");
        assert_eq!(plan.refused, vec![victim.clone()]);
        apply_deletions(&plan, d.path()).unwrap();
        assert!(victim.exists(), "file outside the download dir was deleted");
    }

    #[test]
    fn traversal_in_an_archive_path_is_refused() {
        let d = dir();
        let sneaky = d.path().join("../../etc/passwd");
        let mut a = Archive::new();
        a.insert(1, Some(sneaky.clone()));
        a.insert(2, Some(d.path().join("ok.mp3")));
        write(&d.path().join("ok.mp3"), "x");

        let opts = SyncOptions {
            allow_empty_remote: false,
            max_delete_fraction: 1.0,
        };
        let plan = plan_sync(&a, &[2], d.path(), &opts).unwrap();
        assert!(plan.to_delete.is_empty());
        assert_eq!(plan.refused.len(), 1);
    }

    #[test]
    fn apply_deletions_rechecks_containment() {
        let d = dir();
        let outside = dir();
        let victim = outside.path().join("x.mp3");
        write(&victim, "x");
        // A plan that was tampered with after construction.
        let plan = SyncPlan {
            to_delete: vec![victim.clone()],
            ..Default::default()
        };
        assert!(apply_deletions(&plan, d.path()).is_err());
        assert!(victim.exists());
    }
}
