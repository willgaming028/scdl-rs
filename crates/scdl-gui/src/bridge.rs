//! The bridge between egui's synchronous frame loop and scdl-core's async world.
//!
//! egui's `update()` runs on the UI thread once per frame and must never block.
//! scdl-core is tokio-based. So a tokio runtime lives on its own thread, the UI
//! sends [`Command`]s into it, and it sends [`Update`]s back. Crucially the
//! worker calls `Context::request_repaint()` whenever it pushes an update, so
//! the UI wakes immediately instead of waiting for the next animation tick.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use scdl_core::archive::Archive;
use scdl_core::client::{Client, ClientConfig};
use scdl_core::model::Track;
use scdl_core::pipeline::{download_tracks, DownloadOptions, Event};
use scdl_core::resolve::{resolve, ResolveOptions, Selector, Target};

/// Sent from the UI into the worker.
#[derive(Debug, Clone)]
pub enum Command {
    /// Resolve a URL or search query into a track list.
    Resolve { input: String, selector: Selector },
    /// Download the given tracks.
    Download {
        tracks: Vec<Track>,
        options: Box<DownloadOptions>,
        archive_path: Option<PathBuf>,
    },
    /// Fetch cover art for a track, by id and URL.
    FetchArt { track_id: i64, url: String },
}

/// Sent from the worker back to the UI.
#[derive(Debug)]
pub enum Update {
    Resolving,
    Resolved {
        label: String,
        tracks: Vec<Track>,
    },
    ResolveFailed(String),
    /// A download pipeline event, forwarded verbatim.
    Pipeline(Event),
    /// Decoded cover art, ready to be uploaded as a texture.
    Art {
        track_id: i64,
        rgba: Arc<Vec<u8>>,
        width: u32,
        height: u32,
    },
    /// Something the user should see in the log.
    Note(String),
    Error(String),
}

/// Handle held by the UI.
pub struct Bridge {
    tx: Sender<Command>,
    rx: Receiver<Update>,
}

impl Bridge {
    /// Start the worker thread. `ctx` is cloned so the worker can wake the UI.
    pub fn spawn(ctx: egui::Context, config: ClientConfig) -> Self {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<Command>();
        let (up_tx, up_rx) = std::sync::mpsc::channel::<Update>();

        std::thread::Builder::new()
            .name("scdl-worker".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(4)
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = up_tx.send(Update::Error(format!(
                            "could not start the async runtime: {e}"
                        )));
                        ctx.request_repaint();
                        return;
                    }
                };
                rt.block_on(worker(cmd_rx, up_tx, ctx, config));
            })
            .expect("spawning the worker thread");

        Self {
            tx: cmd_tx,
            rx: up_rx,
        }
    }

    pub fn send(&self, cmd: Command) {
        // A failed send means the worker died; the UI keeps running either way.
        let _ = self.tx.send(cmd);
    }

    /// Drain everything waiting. Never blocks.
    pub fn drain(&self) -> Vec<Update> {
        self.rx.try_iter().collect()
    }
}

async fn worker(
    cmd_rx: Receiver<Command>,
    up_tx: Sender<Update>,
    ctx: egui::Context,
    config: ClientConfig,
) {
    let client = match Client::new(config) {
        Ok(c) => c,
        Err(e) => {
            let _ = up_tx.send(Update::Error(format!("HTTP client: {e}")));
            ctx.request_repaint();
            return;
        }
    };

    // The command channel is blocking, so it is polled on a blocking task and
    // forwarded into an async channel.
    let (async_tx, mut async_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
    std::thread::Builder::new()
        .name("scdl-cmd-pump".into())
        .spawn(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                if async_tx.send(cmd).is_err() {
                    break;
                }
            }
        })
        .expect("spawning the command pump");

    while let Some(cmd) = async_rx.recv().await {
        match cmd {
            Command::Resolve { input, selector } => {
                let client = client.clone();
                let up_tx = up_tx.clone();
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    let _ = up_tx.send(Update::Resolving);
                    ctx.request_repaint();

                    let target = classify(&input);
                    let opts = ResolveOptions {
                        selector,
                        ..Default::default()
                    };
                    let msg = match resolve(&client, &target, &opts).await {
                        Ok(r) => Update::Resolved {
                            label: r.label,
                            tracks: r.tracks,
                        },
                        Err(e) => Update::ResolveFailed(e.to_string()),
                    };
                    let _ = up_tx.send(msg);
                    ctx.request_repaint();
                });
            }

            Command::FetchArt { track_id, url } => {
                let client = client.clone();
                let up_tx = up_tx.clone();
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    if let Some((rgba, w, h)) = fetch_and_decode(&client, &url).await {
                        let _ = up_tx.send(Update::Art {
                            track_id,
                            rgba: Arc::new(rgba),
                            width: w,
                            height: h,
                        });
                        ctx.request_repaint();
                    }
                });
            }

            Command::Download {
                tracks,
                options,
                archive_path,
            } => {
                let client = client.clone();
                let up_tx = up_tx.clone();
                let ctx = ctx.clone();

                tokio::spawn(async move {
                    let archive = match &archive_path {
                        Some(p) => match Archive::load(p) {
                            Ok(a) => Some(Arc::new(tokio::sync::Mutex::new(a))),
                            Err(e) => {
                                let _ = up_tx
                                    .send(Update::Error(format!("archive {}: {e}", p.display())));
                                ctx.request_repaint();
                                None
                            }
                        },
                        None => None,
                    };

                    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();

                    // Forward pipeline events to the UI as they arrive.
                    let forward = {
                        let up_tx = up_tx.clone();
                        let ctx = ctx.clone();
                        tokio::spawn(async move {
                            while let Some(ev) = ev_rx.recv().await {
                                if up_tx.send(Update::Pipeline(ev)).is_err() {
                                    break;
                                }
                                ctx.request_repaint();
                            }
                        })
                    };

                    download_tracks(&client, tracks, &options, archive.clone(), ev_tx).await;
                    let _ = forward.await;

                    if let (Some(arch), Some(path)) = (&archive, &archive_path) {
                        match arch.lock().await.save(path) {
                            Ok(()) => {
                                let _ = up_tx.send(Update::Note(format!(
                                    "archive written to {}",
                                    path.display()
                                )));
                            }
                            Err(e) => {
                                let _ = up_tx
                                    .send(Update::Error(format!("could not write archive: {e}")));
                            }
                        }
                        ctx.request_repaint();
                    }
                });
            }
        }
    }
}

/// A URL if it looks like one, `me` for the signed-in user, otherwise a search.
pub fn classify(s: &str) -> Target {
    let t = s.trim();
    let looks_like_url = t.starts_with("http://")
        || t.starts_with("https://")
        || t.starts_with("soundcloud.com")
        || t.starts_with("www.soundcloud.com")
        || t.starts_with("m.soundcloud.com")
        || t.starts_with("on.soundcloud.com");

    if looks_like_url {
        Target::Url(t.to_string())
    } else if t.eq_ignore_ascii_case("me") {
        Target::Me
    } else {
        Target::Search(t.to_string())
    }
}

/// Download an image and decode it to RGBA8, downscaled to something sane for a
/// texture. Returns `None` on any failure — missing art is never fatal.
async fn fetch_and_decode(client: &Client, url: &str) -> Option<(Vec<u8>, u32, u32)> {
    let resp = client.http().get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().await.ok()?;

    // Decoding is CPU-bound; keep it off the async worker threads.
    tokio::task::spawn_blocking(move || {
        let img = image::load_from_memory(&bytes).ok()?;
        // 500x500 is the largest variant we request; cap anyway in case the
        // "original" art is enormous.
        let img = if img.width() > 640 || img.height() > 640 {
            img.thumbnail(640, 640)
        } else {
            img
        };
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        Some((rgba.into_raw(), w, h))
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_distinguishes_urls_searches_and_me() {
        assert!(matches!(
            classify("https://soundcloud.com/a/b"),
            Target::Url(_)
        ));
        assert!(matches!(classify("soundcloud.com/a/b"), Target::Url(_)));
        assert!(matches!(classify("  me  "), Target::Me));
        assert!(matches!(classify("ME"), Target::Me));
        assert!(matches!(classify("aphex twin"), Target::Search(_)));
        assert!(matches!(classify("boards"), Target::Search(_)));
    }

    #[test]
    fn classify_trims_whitespace() {
        match classify("  https://soundcloud.com/a/b  ") {
            Target::Url(u) => assert_eq!(u, "https://soundcloud.com/a/b"),
            other => panic!("expected Url, got {other:?}"),
        }
    }
}
