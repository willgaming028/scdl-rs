//! Turning parsed arguments into a run.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use tokio::sync::mpsc;

use scdl_core::archive::{apply_deletions, plan_sync, Archive, SyncOptions};
use scdl_core::client::{Client, ClientConfig};
use scdl_core::config::{self, Config};
use scdl_core::pipeline::{download_tracks, DownloadOptions, Event};
use scdl_core::resolve::{resolve, ResolveOptions};
use scdl_core::stream::FormatPreferences;
use scdl_core::tag::TagOptions;

use crate::cli::{parse_size, Cli};

pub async fn main(args: Cli) -> Result<()> {
    let config_path = config::default_config_path();
    let cfg =
        Config::load(&config_path).with_context(|| format!("reading {}", config_path.display()))?;

    if let Some(mode) = config::insecure_permissions(&config_path) {
        if cfg.has_secret() {
            eprintln!(
                "warning: {} holds an auth token but is mode {:o}; run `chmod 600 {}`",
                config_path.display(),
                mode,
                config_path.display()
            );
        }
    }

    let auth_token = args.auth_token.clone().or_else(|| cfg.auth_token.clone());
    let client_id = args.client_id.clone().or_else(|| cfg.client_id.clone());

    let client = Client::new(ClientConfig {
        client_id,
        auth_token: auth_token.clone(),
        timeout: None,
    })
    .context("could not build the HTTP client")?;

    if let Some(token) = &auth_token {
        match client.verify_auth_token(token).await {
            Ok(true) => {}
            Ok(false) => eprintln!("warning: the auth token was rejected; continuing as a guest"),
            Err(e) => eprintln!("warning: could not verify the auth token ({e}); continuing"),
        }
    }

    let output_dir = args
        .path
        .clone()
        .unwrap_or_else(|| cfg.path.clone())
        .canonicalize()
        .unwrap_or_else(|_| args.path.clone().unwrap_or_else(|| cfg.path.clone()));

    let opts = build_options(&args, &cfg, output_dir.clone())?;

    // Persist a freshly scraped client_id so the next run skips the scrape.
    if cfg.client_id.is_none() {
        if let Ok(id) = client.client_id().await {
            let mut updated = cfg.clone();
            updated.client_id = Some(id);
            let _ = updated.save(&config_path);
        }
    }

    let archive_path = args.download_archive.clone().or_else(|| args.sync.clone());
    let archive = match &archive_path {
        Some(p) => Some(Arc::new(tokio::sync::Mutex::new(
            Archive::load(p).with_context(|| format!("reading archive {}", p.display()))?,
        ))),
        None => None,
    };

    if args.should_use_tui() {
        return crate::tui::run(
            client,
            output_dir,
            opts,
            archive,
            archive_path,
            args.target(),
        )
        .await;
    }

    let Some(target) = args.target() else {
        bail!("nothing to do: pass -l <url>, -s <query>, or `me` (or run with no arguments for the UI)");
    };

    let resolve_opts = ResolveOptions {
        selector: args.selector(),
        offset: args.offset,
        limit: None,
        no_playlist: args.no_playlist,
    };

    if !args.error {
        eprintln!("resolving…");
    }
    let resolved = resolve(&client, &target, &resolve_opts)
        .await
        .context("could not resolve that target")?;

    if resolved.tracks.is_empty() {
        bail!("{}: nothing to download", resolved.label);
    }
    if !args.error {
        eprintln!("{} — {} track(s)", resolved.label, resolved.tracks.len());
    }

    // --- sync: plan deletions before downloading anything ---
    let mut sync_plan = None;
    if let (Some(sync_path), Some(arch)) = (&args.sync, &archive) {
        let remote_ids: Vec<i64> = resolved.tracks.iter().map(|t| t.id).collect();
        let sync_opts = SyncOptions {
            allow_empty_remote: args.sync_allow_empty,
            max_delete_fraction: if args.sync_force { 1.0 } else { 0.5 },
        };

        let plan = plan_sync(&*arch.lock().await, &remote_ids, &output_dir, &sync_opts)
            .context("refusing to sync")?;

        for p in &plan.refused {
            eprintln!(
                "warning: archive lists {} which is outside {}; refusing to delete it",
                p.display(),
                output_dir.display()
            );
        }

        if args.dry_run {
            println!("dry run — {} would do:", sync_path.display());
            println!("  download {} new track(s)", plan.to_download.len());
            for p in &plan.to_delete {
                println!("  delete {}", p.display());
            }
            if plan.is_noop() {
                println!("  (nothing to do)");
            }
            return Ok(());
        }
        sync_plan = Some(plan);
    }

    // --- download ---
    let (tx, rx) = mpsc::unbounded_channel::<Event>();
    let total = resolved.tracks.len();
    let render = tokio::spawn(render_progress(rx, total, args.hide_progress || args.error));

    let summary = download_tracks(&client, resolved.tracks, &opts, archive.clone(), tx).await;
    let _ = render.await;

    // --- sync: apply deletions only after a successful run ---
    if let (Some(plan), Some(arch)) = (sync_plan, &archive) {
        let removed = apply_deletions(&plan, &output_dir).context("applying sync deletions")?;
        for p in &removed {
            eprintln!("removed {}", p.display());
        }
        let mut guard = arch.lock().await;
        // Collect first: `entries()` borrows the archive immutably and `remove`
        // needs it mutably.
        let removed_ids: Vec<i64> = guard
            .entries()
            .filter(|e| {
                e.path
                    .as_deref()
                    .is_some_and(|p| removed.iter().any(|r| r == p))
            })
            .filter_map(|e| e.id.parse::<i64>().ok())
            .collect();
        for id in plan.stale_ids.into_iter().chain(removed_ids) {
            guard.remove(id);
        }
    }

    if let (Some(arch), Some(path)) = (&archive, &archive_path) {
        arch.lock()
            .await
            .save(path)
            .with_context(|| format!("writing archive {}", path.display()))?;
    }

    if !args.error {
        eprintln!(
            "done — {} downloaded, {} skipped, {} failed",
            summary.completed, summary.skipped, summary.failed
        );
    }

    if summary.failed > 0 && args.strict {
        bail!(
            "{} track(s) failed and --strict-playlist was given",
            summary.failed
        );
    }
    Ok(())
}

fn build_options(args: &Cli, cfg: &Config, output_dir: PathBuf) -> Result<DownloadOptions> {
    let min_size = args
        .min_size
        .as_deref()
        .map(parse_size)
        .transpose()
        .map_err(|e| anyhow::anyhow!("--min-size: {e}"))?;
    let max_size = args
        .max_size
        .as_deref()
        .map(parse_size)
        .transpose()
        .map_err(|e| anyhow::anyhow!("--max-size: {e}"))?;

    if let (Some(lo), Some(hi)) = (min_size, max_size) {
        if lo > hi {
            bail!("--min-size is larger than --max-size");
        }
    }

    Ok(DownloadOptions {
        output_dir,
        name_format: args
            .name_format
            .clone()
            .unwrap_or_else(|| cfg.name_format.clone()),
        playlist_name_format: args
            .playlist_name_format
            .clone()
            .unwrap_or_else(|| cfg.playlist_name_format.clone()),
        no_playlist_folder: args.no_playlist_folder,
        format_prefs: FormatPreferences {
            only_mp3: args.only_mp3,
            allow_opus: args.opus,
            no_original: args.no_original,
            only_original: args.only_original,
        },
        tag_opts: TagOptions {
            extract_artist: args.extract_artist,
            no_album_tag: args.no_album_tag,
            skip: args.original_metadata,
        },
        continue_existing: args.continue_existing,
        overwrite: args.overwrite,
        force_metadata: args.force_metadata,
        original_art: args.original_art,
        add_description: args.add_description,
        min_size,
        max_size,
        flac: args.flac,
        original_name: args.original_name,
        addtofile: args.addtofile,
        addtimestamp: args.addtimestamp,
        concurrency: args.jobs.clamp(1, 16),
        strict: args.strict,
    })
}

/// Plain-CLI progress rendering with indicatif.
async fn render_progress(mut rx: mpsc::UnboundedReceiver<Event>, total: usize, quiet: bool) {
    if quiet {
        // Still surface failures: suppressing progress should not suppress the
        // reason a track did not download.
        while let Some(ev) = rx.recv().await {
            if let Event::Failed { error, .. } = ev {
                eprintln!("  error: {error}");
            }
        }
        return;
    }

    let multi = MultiProgress::new();
    let overall = multi.add(ProgressBar::new(total as u64));
    overall.set_style(
        ProgressStyle::with_template("{spinner:.yellow} [{bar:30.yellow/dim}] {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("━╸─"),
    );

    let bar_style = ProgressStyle::with_template(
        "  {spinner:.yellow} {msg:38!} [{bar:22.yellow/dim}] {bytes:>9} {bytes_per_sec:>11} {eta:>5}",
    )
    .unwrap()
    .progress_chars("━╸─");

    let mut bars: std::collections::HashMap<usize, ProgressBar> = Default::default();
    let mut titles: std::collections::HashMap<usize, String> = Default::default();

    while let Some(ev) = rx.recv().await {
        match ev {
            Event::Queued { index, title, .. } => {
                titles.insert(index, title);
            }
            Event::Started { index, .. } => {
                let pb = multi.insert_before(&overall, ProgressBar::new(0));
                pb.set_style(bar_style.clone());
                pb.set_message(titles.get(&index).cloned().unwrap_or_default());
                pb.enable_steady_tick(std::time::Duration::from_millis(120));
                bars.insert(index, pb);
            }
            Event::Progress {
                index,
                downloaded,
                total,
            } => {
                if let Some(pb) = bars.get(&index) {
                    if let Some(t) = total {
                        pb.set_length(t);
                    }
                    pb.set_position(downloaded);
                }
            }
            Event::Finished { index, bytes, .. } => {
                if let Some(pb) = bars.remove(&index) {
                    pb.set_length(bytes.max(1));
                    pb.set_position(bytes.max(1));
                    pb.finish_and_clear();
                }
                let title = titles.get(&index).cloned().unwrap_or_default();
                overall.inc(1);
                overall.println(format!("  ✓ {title}"));
            }
            Event::Skipped { index, reason } => {
                if let Some(pb) = bars.remove(&index) {
                    pb.finish_and_clear();
                }
                let title = titles.get(&index).cloned().unwrap_or_default();
                overall.inc(1);
                overall.println(format!("  – {title} ({reason})"));
            }
            Event::Failed { index, error } => {
                if let Some(pb) = bars.remove(&index) {
                    pb.finish_and_clear();
                }
                let title = titles.get(&index).cloned().unwrap_or_default();
                overall.inc(1);
                // Both: println keeps it in order above the bars, and stderr
                // guarantees it survives the bars being cleared at the end.
                overall.println(format!("  ✗ {title}: {error}"));
                overall.suspend(|| eprintln!("  ✗ {title}: {error}"));
            }
            Event::Done { .. } => break,
            _ => {}
        }
    }

    overall.finish_and_clear();
}
