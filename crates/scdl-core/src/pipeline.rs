//! The download pipeline: everything between "here is a track" and "here is a
//! tagged file on disk".
//!
//! Both front-ends drive this. It reports progress as a stream of [`Event`]s
//! over a channel rather than printing, so the CLI can render bars and the TUI
//! can render a live queue from exactly the same run.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{mpsc, Semaphore};

use crate::archive::Archive;
use crate::client::Client;
use crate::download::{download_stream, Progress, ProgressSink};
use crate::error::{Error, Result};
use crate::model::Track;
use crate::naming::{build_output_path, Fields};
use crate::stream::{available_formats, original_download, resolve_stream_url, FormatPreferences};
use crate::tag::{fetch_artwork, write_tags, Metadata, TagOptions};

/// Settings for a whole run.
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub output_dir: PathBuf,
    pub name_format: String,
    pub playlist_name_format: String,
    /// `--no-playlist-folder`: write playlist tracks into `output_dir` directly.
    pub no_playlist_folder: bool,
    pub format_prefs: FormatPreferences,
    pub tag_opts: TagOptions,
    /// `-c`: skip a track whose file already exists.
    pub continue_existing: bool,
    /// `--overwrite`: replace an existing file.
    pub overwrite: bool,
    /// `--force-metadata`: re-tag even when skipping the download.
    pub force_metadata: bool,
    /// `--original-art`: embed the full-size cover instead of the 500x500 JPEG.
    pub original_art: bool,
    /// `--add-description`: write a sidecar `.txt` next to the audio.
    pub add_description: bool,
    /// `--onlymp3` etc. are inside `format_prefs`; this is `--min-size`/`--max-size`.
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    /// `--flac`: re-encode a lossless original to FLAC.
    pub flac: bool,
    /// `--original-name`: keep the uploader's own filename for original downloads.
    pub original_name: bool,
    /// `--addtofile`: prefix the filename with the artist (legacy).
    pub addtofile: bool,
    /// `--addtimestamp`: prefix the filename with the upload timestamp (legacy).
    pub addtimestamp: bool,
    /// How many tracks to download at once.
    pub concurrency: usize,
    /// `--strict-playlist`: abort the whole run on the first failure.
    pub strict: bool,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            output_dir: PathBuf::from("."),
            name_format: crate::naming::DEFAULT_NAME_FORMAT.to_string(),
            playlist_name_format: crate::naming::DEFAULT_PLAYLIST_NAME_FORMAT.to_string(),
            no_playlist_folder: false,
            format_prefs: FormatPreferences::default(),
            tag_opts: TagOptions::default(),
            continue_existing: false,
            overwrite: false,
            force_metadata: false,
            original_art: false,
            add_description: false,
            min_size: None,
            max_size: None,
            flac: false,
            original_name: false,
            addtofile: false,
            addtimestamp: false,
            concurrency: 3,
            strict: false,
        }
    }
}

/// Progress and lifecycle events for one run.
#[derive(Debug, Clone)]
pub enum Event {
    /// A track entered the queue. `index` is its position in the run.
    Queued {
        index: usize,
        id: i64,
        title: String,
        artist: String,
    },
    Resolving {
        index: usize,
    },
    Started {
        index: usize,
        path: PathBuf,
        format: String,
    },
    Progress {
        index: usize,
        downloaded: u64,
        total: Option<u64>,
    },
    Tagging {
        index: usize,
    },
    Finished {
        index: usize,
        path: PathBuf,
        bytes: u64,
    },
    Skipped {
        index: usize,
        reason: String,
    },
    Failed {
        index: usize,
        error: String,
    },
    /// The whole run is over.
    Done {
        completed: usize,
        skipped: usize,
        failed: usize,
    },
}

/// Outcome of one track.
#[derive(Debug, Clone)]
pub enum TrackOutcome {
    Downloaded { path: PathBuf, bytes: u64 },
    Skipped(String),
    Failed(String),
}

struct ChannelSink {
    index: usize,
    tx: mpsc::UnboundedSender<Event>,
}

impl ProgressSink for ChannelSink {
    fn on_progress(&self, p: Progress) {
        let _ = self.tx.send(Event::Progress {
            index: self.index,
            downloaded: p.downloaded,
            total: p.total,
        });
    }
}

/// A shared stop flag for a run.
///
/// Granularity is per track: a cancelled run stops *starting* tracks and lets
/// in-flight ones finish their current file. That is deliberate — tearing down
/// mid-write would leave partial files, and a track takes seconds, not minutes.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn reset(&self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

/// Summary of a completed run.
#[derive(Debug, Clone, Default)]
pub struct RunSummary {
    pub completed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub outcomes: Vec<(i64, TrackOutcome)>,
}

/// Download a list of tracks.
///
/// `archive`, when supplied, is consulted to skip already-downloaded tracks and
/// updated as tracks complete. It is the caller's job to persist it — doing so
/// here would mean writing the file once per track.
pub async fn download_tracks(
    client: &Client,
    tracks: Vec<Track>,
    opts: &DownloadOptions,
    archive: Option<Arc<tokio::sync::Mutex<Archive>>>,
    tx: mpsc::UnboundedSender<Event>,
) -> RunSummary {
    download_tracks_cancellable(client, tracks, opts, archive, tx, Cancel::new()).await
}

/// As [`download_tracks`], but stoppable through a [`Cancel`] token.
pub async fn download_tracks_cancellable(
    client: &Client,
    tracks: Vec<Track>,
    opts: &DownloadOptions,
    archive: Option<Arc<tokio::sync::Mutex<Archive>>>,
    tx: mpsc::UnboundedSender<Event>,
    cancel: Cancel,
) -> RunSummary {
    for (i, t) in tracks.iter().enumerate() {
        let _ = tx.send(Event::Queued {
            index: i,
            id: t.id,
            title: t.title_or_untitled().to_string(),
            artist: t.artist().to_string(),
        });
    }

    let semaphore = Arc::new(Semaphore::new(opts.concurrency.max(1)));
    let mut handles = Vec::new();

    for (index, track) in tracks.into_iter().enumerate() {
        if cancel.is_cancelled() {
            let _ = tx.send(Event::Skipped {
                index,
                reason: "cancelled".to_string(),
            });
            continue;
        }
        let permit = match Arc::clone(&semaphore).acquire_owned().await {
            Ok(p) => p,
            Err(_) => break,
        };
        let client = client.clone();
        let opts = opts.clone();
        let archive = archive.clone();
        let tx = tx.clone();

        let cancel = cancel.clone();
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let id = track.id;
            // Re-check after waiting for a slot: the user may have cancelled
            // while this task sat in the queue.
            let outcome = if cancel.is_cancelled() {
                TrackOutcome::Skipped("cancelled".to_string())
            } else {
                download_one(&client, &track, &opts, archive.clone(), index, &tx).await
            };

            match &outcome {
                TrackOutcome::Downloaded { path, bytes } => {
                    let _ = tx.send(Event::Finished {
                        index,
                        path: path.clone(),
                        bytes: *bytes,
                    });
                }
                TrackOutcome::Skipped(reason) => {
                    let _ = tx.send(Event::Skipped {
                        index,
                        reason: reason.clone(),
                    });
                }
                TrackOutcome::Failed(error) => {
                    let _ = tx.send(Event::Failed {
                        index,
                        error: error.clone(),
                    });
                }
            }
            (id, outcome)
        }));
    }

    let mut summary = RunSummary::default();
    for h in handles {
        if let Ok((id, outcome)) = h.await {
            match &outcome {
                TrackOutcome::Downloaded { .. } => summary.completed += 1,
                TrackOutcome::Skipped(_) => summary.skipped += 1,
                TrackOutcome::Failed(_) => summary.failed += 1,
            }
            summary.outcomes.push((id, outcome));
        }
    }

    let _ = tx.send(Event::Done {
        completed: summary.completed,
        skipped: summary.skipped,
        failed: summary.failed,
    });

    summary
}

async fn download_one(
    client: &Client,
    track: &Track,
    opts: &DownloadOptions,
    archive: Option<Arc<tokio::sync::Mutex<Archive>>>,
    index: usize,
    tx: &mpsc::UnboundedSender<Event>,
) -> TrackOutcome {
    match download_one_inner(client, track, opts, archive, index, tx).await {
        Ok(o) => o,
        Err(e) => TrackOutcome::Failed(e.to_string()),
    }
}

async fn download_one_inner(
    client: &Client,
    track: &Track,
    opts: &DownloadOptions,
    archive: Option<Arc<tokio::sync::Mutex<Archive>>>,
    index: usize,
    tx: &mpsc::UnboundedSender<Event>,
) -> Result<TrackOutcome> {
    let _ = tx.send(Event::Resolving { index });

    if let Some(arch) = archive.as_deref() {
        if arch.lock().await.contains(track.id) {
            return Ok(TrackOutcome::Skipped("in download archive".to_string()));
        }
    }

    if track.is_geo_blocked() {
        return Ok(TrackOutcome::Skipped("blocked in your region".to_string()));
    }

    // Prefer the uploader's original file when they have allowed it and the
    // user has not opted out.
    let original = if opts.format_prefs.no_original {
        None
    } else {
        original_download(client, track).await.unwrap_or(None)
    };

    if opts.format_prefs.only_original && original.is_none() {
        return Ok(TrackOutcome::Skipped(
            "no original file available".to_string(),
        ));
    }

    let mut original_filename: Option<String> = None;
    let (stream, ext) = match original {
        Some(orig) => {
            // The server names the file; fall back to a sensible default. Only
            // the extension is trusted unless --original-name was asked for.
            let probed = probe_original(client, &orig.url).await;
            let ext = probed
                .as_ref()
                .and_then(|p| p.ext.clone())
                .unwrap_or_else(|| "mp3".into());
            if opts.original_name {
                original_filename = probed.and_then(|p| p.filename);
            }
            (
                crate::stream::ResolvedStream {
                    url: orig.url,
                    format: crate::model::Format {
                        transcoding_url: String::new(),
                        protocol: crate::model::Protocol::Http,
                        codec: crate::model::Codec::Original,
                        preset: "original".to_string(),
                        ext: "bin",
                        abr: None,
                        is_preview: false,
                        is_premium: false,
                    },
                },
                ext,
            )
        }
        None => {
            // Try every acceptable format in preference order. SoundCloud
            // regularly 404s an individual transcoding's resolver even when the
            // track itself is fine, so giving up after the first one loses
            // tracks that are perfectly downloadable via the next format.
            let formats = available_formats(track, &opts.format_prefs)?;
            let mut last_err = None;
            let mut chosen = None;

            for format in formats {
                if format.is_preview {
                    // Only a 30-second snippet is on offer; do not save that as
                    // if it were the track.
                    last_err = Some(Error::Snipped {
                        title: track.title_or_untitled().to_string(),
                    });
                    continue;
                }
                match resolve_stream_url(client, track, &format).await {
                    Ok(resolved) => {
                        chosen = Some((resolved, format.ext.to_string()));
                        break;
                    }
                    Err(e) => last_err = Some(e),
                }
            }

            match chosen {
                Some(pair) => pair,
                None => {
                    // If every usable format failed to resolve AND the track also
                    // carries DRM transcodings, DRM is the real cause; reporting
                    // the raw 404 would send the user chasing a network problem.
                    if crate::stream::has_drm_transcodings(track) {
                        return Err(Error::Drm {
                            title: track.title_or_untitled().to_string(),
                        });
                    }
                    return Err(last_err.unwrap_or(Error::NoFormat {
                        title: track.title_or_untitled().to_string(),
                    }));
                }
            }
        }
    };

    let in_playlist = track.playlist_context.is_some();
    let base_template = if in_playlist {
        &opts.playlist_name_format
    } else {
        &opts.name_format
    };

    // Legacy --addtofile / --addtimestamp replace the template outright, exactly
    // as the Python version does, rather than composing with it.
    let legacy_template;
    let template = if opts.addtofile || opts.addtimestamp {
        let mut t = String::from("%(title)s.%(ext)s");
        if opts.addtofile {
            t = format!("%(uploader)s - {t}");
        }
        if opts.addtimestamp {
            t = format!("%(timestamp)s_{t}");
        }
        legacy_template = t;
        &legacy_template
    } else if let Some(name) = &original_filename {
        // --original-name: the uploader's filename, with the extension we probed.
        let stem = std::path::Path::new(name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(name);
        // Escape any '%' so the uploader's filename cannot inject a field.
        legacy_template = format!("{}.%(ext)s", stem.replace('%', "%%"));
        &legacy_template
    } else {
        base_template
    };

    let fields = Fields::from_track(track, &ext);
    let subfolder = if in_playlist && !opts.no_playlist_folder {
        track.playlist_context.as_ref().map(|c| c.title.as_str())
    } else {
        None
    };

    let dest = build_output_path(&opts.output_dir, template, &fields, subfolder)?;

    if dest.exists() && !opts.overwrite {
        if opts.force_metadata {
            let _ = tx.send(Event::Tagging { index });
            apply_tags(client, track, opts, &dest).await?;
            return Ok(TrackOutcome::Skipped(
                "already downloaded (metadata refreshed)".to_string(),
            ));
        }
        if opts.continue_existing {
            return Ok(TrackOutcome::Skipped("already downloaded".to_string()));
        }
        return Ok(TrackOutcome::Skipped(
            "file exists (use --overwrite or -c)".to_string(),
        ));
    }

    let _ = tx.send(Event::Started {
        index,
        path: dest.clone(),
        format: stream.format.short_id(),
    });

    let sink: Arc<dyn ProgressSink> = Arc::new(ChannelSink {
        index,
        tx: tx.clone(),
    });
    let done = download_stream(client, &stream, &dest, sink).await?;

    // Size filters are applied after the fact because SoundCloud does not
    // reliably advertise a length up front for HLS.
    if let Some(min) = opts.min_size {
        if done.bytes < min {
            let _ = tokio::fs::remove_file(&dest).await;
            return Ok(TrackOutcome::Skipped(format!(
                "smaller than minimum size ({} bytes)",
                done.bytes
            )));
        }
    }
    if let Some(max) = opts.max_size {
        if done.bytes > max {
            let _ = tokio::fs::remove_file(&dest).await;
            return Ok(TrackOutcome::Skipped(format!(
                "larger than maximum size ({} bytes)",
                done.bytes
            )));
        }
    }

    // --flac: only meaningful for a lossless original; re-encoding a lossy
    // stream to FLAC would just make a bigger file with the same quality.
    let dest = if opts.flac && matches!(ext.as_str(), "aiff" | "aif" | "wav" | "alac" | "flac") {
        recode_to_flac(&dest).await.unwrap_or(dest)
    } else {
        dest
    };

    if !opts.tag_opts.skip {
        let _ = tx.send(Event::Tagging { index });
        apply_tags(client, track, opts, &dest).await?;
    }

    if opts.add_description {
        write_description(track, &dest).await?;
    }

    if let Some(arch) = archive.as_deref() {
        arch.lock().await.insert(track.id, Some(dest.clone()));
    }

    Ok(TrackOutcome::Downloaded {
        path: dest,
        bytes: done.bytes,
    })
}

async fn apply_tags(
    client: &Client,
    track: &Track,
    opts: &DownloadOptions,
    dest: &Path,
) -> Result<()> {
    let mut meta = Metadata::from_track(track, &opts.tag_opts);
    meta.artwork = fetch_artwork(client, track, opts.original_art)
        .await
        .unwrap_or(None);

    // Tagging failures should not discard a good download; report and move on.
    match write_tags(dest, &meta) {
        Ok(()) => Ok(()),
        Err(Error::Tag { .. }) => Ok(()),
        Err(e) => Err(e),
    }
}

/// `--add-description`: a sidecar `.txt` beside the audio file.
///
/// Uses the audio file's own path with the extension swapped, so a playlist's
/// descriptions land next to their tracks rather than all colliding in the base
/// directory (which is what the Python version does).
async fn write_description(track: &Track, dest: &Path) -> Result<()> {
    let Some(desc) = track.description.as_deref().filter(|d| !d.is_empty()) else {
        return Ok(());
    };
    let mut txt = dest.to_path_buf();
    txt.set_extension("txt");
    tokio::fs::write(&txt, desc)
        .await
        .map_err(|e| Error::io(&txt, e))
}

/// What a HEAD against the original-download URL told us.
struct OriginalProbe {
    /// Lowercase, alphanumeric-only extension.
    ext: Option<String>,
    /// The uploader's filename, sanitized to a single path component.
    filename: Option<String>,
}

/// Ask the origin what the original file is, via a HEAD request.
///
/// The filename is remote-controlled, so it is run through
/// `sanitize_component` here and can never contribute a path separator or a
/// directory reference. The Python version splices it into its output template
/// unsanitized.
async fn probe_original(client: &Client, url: &str) -> Option<OriginalProbe> {
    let resp = client.http().head(url).send().await.ok()?;

    let raw_name = resp
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .and_then(filename_from_content_disposition);

    let ext_from_name = raw_name.as_deref().and_then(|name| {
        Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|ext| {
                ext.chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .take(5)
                    .collect::<String>()
                    .to_ascii_lowercase()
            })
    });

    let ext_from_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .and_then(|ct| ct.split(';').next())
        .map(str::trim)
        .and_then(|ct| {
            Some(
                match ct {
                    "audio/mpeg" => "mp3",
                    "audio/mp4" | "audio/x-m4a" => "m4a",
                    "audio/flac" | "audio/x-flac" => "flac",
                    "audio/wav" | "audio/x-wav" => "wav",
                    "audio/aiff" | "audio/x-aiff" => "aiff",
                    "audio/ogg" => "ogg",
                    _ => return None,
                }
                .to_string(),
            )
        });

    Some(OriginalProbe {
        ext: ext_from_name.filter(|e| !e.is_empty()).or(ext_from_type),
        filename: raw_name.map(|n| crate::naming::sanitize_component(&n)),
    })
}

/// Re-encode to FLAC beside the source, replacing it on success.
async fn recode_to_flac(src: &Path) -> Result<PathBuf> {
    let dest = src.with_extension("flac");
    if dest == src {
        return Ok(dest);
    }
    crate::download::recode_flac(src, &dest).await?;
    let _ = tokio::fs::remove_file(src).await;
    Ok(dest)
}

fn filename_from_content_disposition(value: &str) -> Option<String> {
    for part in value.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("filename=") {
            return Some(rest.trim_matches('"').to_string());
        }
        if let Some(rest) = part.strip_prefix("filename*=") {
            // RFC 5987: charset'lang'percent-encoded-value
            let encoded = rest.rsplit('\'').next()?;
            return Some(
                percent_encoding::percent_decode_str(encoded)
                    .decode_utf8_lossy()
                    .to_string(),
            );
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition_plain_filename() {
        assert_eq!(
            filename_from_content_disposition(r#"attachment; filename="My Track.wav""#).as_deref(),
            Some("My Track.wav")
        );
    }

    #[test]
    fn content_disposition_rfc5987() {
        assert_eq!(
            filename_from_content_disposition("attachment; filename*=UTF-8''My%20Track.flac")
                .as_deref(),
            Some("My Track.flac")
        );
    }

    #[test]
    fn content_disposition_without_filename() {
        assert_eq!(filename_from_content_disposition("inline"), None);
    }

    #[test]
    fn description_sidecar_sits_beside_its_audio_file() {
        // The Python version writes all playlist descriptions to the base
        // directory, where a custom name format can collide them into one file.
        let audio = Path::new("/music/The Lost Ship/03. Track.m4a");
        let mut txt = audio.to_path_buf();
        txt.set_extension("txt");
        assert_eq!(txt, Path::new("/music/The Lost Ship/03. Track.txt"));
    }
}

#[cfg(test)]
mod cancel_tests {
    use super::*;

    #[test]
    fn cancel_token_is_shared_between_clones() {
        let a = Cancel::new();
        let b = a.clone();
        assert!(!a.is_cancelled() && !b.is_cancelled());
        b.cancel();
        assert!(a.is_cancelled(), "cancel must be visible through a clone");
        a.reset();
        assert!(!b.is_cancelled());
    }

    #[tokio::test]
    async fn a_cancelled_run_downloads_nothing() {
        let client = Client::new(crate::client::ClientConfig::default()).unwrap();
        let tracks: Vec<Track> = (0..5)
            .map(|i| {
                let mut t: Track = serde_json::from_str(r#"{"id":0}"#).unwrap();
                t.id = i;
                t.title = Some(format!("t{i}"));
                t
            })
            .collect();

        let cancel = Cancel::new();
        cancel.cancel();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let summary = download_tracks_cancellable(
            &client,
            tracks,
            &DownloadOptions::default(),
            None,
            tx,
            cancel,
        )
        .await;

        // Nothing hit the network; every track was skipped as cancelled.
        assert_eq!(summary.completed, 0);
        assert_eq!(summary.failed, 0);

        let mut cancelled = 0;
        while let Ok(ev) = rx.try_recv() {
            if let Event::Skipped { reason, .. } = ev {
                if reason == "cancelled" {
                    cancelled += 1;
                }
            }
        }
        assert_eq!(cancelled, 5, "every track should report as cancelled");
    }
}
