//! scdl — a SoundCloud downloader.
//!
//! Two front-ends over one core: a plain CLI that mirrors the original tool's
//! flags, and an interactive terminal UI that opens when no target is given.

mod cli;
mod run;
mod tui;

use anyhow::{Context, Result};
use clap::Parser;

use cli::Cli;

fn main() -> Result<()> {
    let args = Cli::parse();

    // A multi-threaded runtime: HLS segment fetching and per-track concurrency
    // both want real parallelism, and ffmpeg remuxes block a worker.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")?;

    runtime.block_on(run::main(args))
}
