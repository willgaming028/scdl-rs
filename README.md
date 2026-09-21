# scdl-rs

A SoundCloud downloader in pure Rust — a CLI, a terminal UI, and an animated desktop app.

A clean-room reimplementation of [scdl](https://github.com/scdl-org/scdl). It talks to
SoundCloud's v2 API directly: **no Python, no yt-dlp**. `ffmpeg` is needed only to remux AAC
streams and for `--flac`.

---

## Quick start

Pick your platform, paste the block, done.

### Linux (Debian / Ubuntu / Mint / Pop!_OS)

```bash
sudo apt update && sudo apt install -y build-essential ffmpeg git curl
```

The desktop app also needs GL and X11/Wayland libraries at runtime. Any desktop install already
has them; on a minimal or server install add:

```bash
sudo apt install -y libgl1 libx11-6 libxcursor1 libxkbcommon-x11-0 libwayland-client0
```

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && . "$HOME/.cargo/env"
```

```bash
git clone https://github.com/willgaming028/scdl-rs.git && cd scdl-rs && cargo build --release
```

```bash
install -Dm755 target/release/scdl ~/.local/bin/scdl && install -Dm755 target/release/scdl-gui ~/.local/bin/scdl-gui
```

### Linux (Fedora / RHEL)

```bash
sudo dnf install -y gcc gcc-c++ ffmpeg git curl
```

Minimal installs also need the GUI's runtime libraries:

```bash
sudo dnf install -y mesa-libGL libX11 libXcursor libxkbcommon-x11 libwayland-client
```

Then the same three `rustup` / `git clone` / `install` commands as above.

### Linux (Arch / Manjaro)

```bash
sudo pacman -S --needed base-devel ffmpeg git curl
```

Minimal installs also need the GUI's runtime libraries:

```bash
sudo pacman -S --needed libgl libx11 libxcursor libxkbcommon-x11 wayland
```

Then the same three `rustup` / `git clone` / `install` commands as above.

### macOS

```bash
xcode-select --install
```

```bash
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
```

```bash
brew install ffmpeg git
```

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && . "$HOME/.cargo/env"
```

```bash
git clone https://github.com/willgaming028/scdl-rs.git && cd scdl-rs && cargo build --release
```

```bash
sudo cp target/release/scdl target/release/scdl-gui /usr/local/bin/
```

### Windows (PowerShell)

```powershell
winget install --id Rustlang.Rustup -e --accept-source-agreements --accept-package-agreements
```

```powershell
winget install --id Gyan.FFmpeg -e ; winget install --id Git.Git -e
```

```powershell
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

(The Build Tools supply the MSVC linker. Nothing else native is needed — TLS is pure Rust.)

Close and reopen PowerShell so the new `PATH` takes effect, then:

```powershell
git clone https://github.com/willgaming028/scdl-rs.git ; cd scdl-rs ; cargo build --release
```

The two binaries land in `target\release\`. To put them on your `PATH`:

```powershell
New-Item -ItemType Directory -Force "$env:LOCALAPPDATA\Programs\scdl" ; Copy-Item target\release\scdl.exe,target\release\scdl-gui.exe "$env:LOCALAPPDATA\Programs\scdl\" ; [Environment]::SetEnvironmentVariable("Path", [Environment]::GetEnvironmentVariable("Path","User") + ";$env:LOCALAPPDATA\Programs\scdl", "User")
```

### What the build actually needs

**Rust 1.95 or newer** (egui/eframe require it; `rustup update` if in doubt).

Deliberately kept small: a C compiler and a linker, and that is it. TLS is
[rustls](https://github.com/rustls/rustls) with the `ring` provider, so there is **no OpenSSL**;
no dependency uses `pkg-config` or `cmake`. The finished binaries link only `libc`, `libgcc` and
`libm` — the GUI loads GL, X11, Wayland and xkbcommon dynamically at startup.

`ffmpeg` is a *runtime* dependency, and only for remuxing AAC downloads and for `--flac`.
`--onlymp3` avoids needing it at all.

### Build only, no install

```bash
cargo build --release
```

Binaries: `target/release/scdl` (CLI + TUI) and `target/release/scdl-gui` (desktop app).

### Check it works

```bash
scdl --version && scdl-gui --version && ffmpeg -version | head -1
```

---

## Using it

### Desktop app

```bash
scdl-gui
```

Want a look around without touching the network?

```bash
scdl-gui --demo
```

### Terminal UI

```bash
scdl
```

Paste a URL, Enter to resolve, Space to toggle tracks, Enter to download, `?` for keys, `q` to quit.

### Command line

```bash
scdl -l https://soundcloud.com/pandadub/sets/the-lost-ship
```

```bash
scdl -l https://soundcloud.com/quanta-uk -a
```

```bash
scdl -l https://soundcloud.com/kobiblastoyz -f
```

```bash
scdl -s "aphex twin"
```

```bash
scdl me -f
```

Every flag from the original works, plus `-j/--jobs` for parallel downloads, `--tui`/`--no-tui`,
and `--dry-run`. `--yt-dlp-args` is the only one dropped — there is no yt-dlp to forward to.

---

## The three front-ends

| | what it is | when to use it |
|---|---|---|
| `scdl -l <url>` | plain CLI with progress bars | scripts, cron, SSH |
| `scdl` | full terminal UI | browsing over SSH, no desktop |
| `scdl-gui` | animated desktop app | day-to-day use |

All three drive the same `scdl-core`, so they behave identically — only the presentation differs.

---

## The desktop app

Four views behind a nav rail whose active indicator slides between them.

**Browse** — search field, selector chips (uploads / likes / reposts / playlists / commented),
result cards with cover art that fade and slide in on a stagger, each lifting on hover.

**Queue** — a hero panel for the current download: animated progress ring, live
bytes / speed / ETA, a throughput sparkline with a gradient fill (real data), and a decorative
spectrum along the bottom. Below it, one animated card per track with its own ring and a progress hairline. A log
drawer sits underneath for the detail behind a failure.

**Library** — everything downloaded this session, with thumbnails.

**Settings** — bound to the real config: output folder, both name formats, parallel-download
slider, format toggles, auth token (masked), theme, and an animation switch.

A run in progress can be stopped with the Cancel button in the Queue header.

### The animation layer

Written by hand in `anim.rs` and `icons.rs` — no animation crate.

- **Aurora background** — a drifting multi-stop gradient, built as an `epaint::Mesh` with
  per-vertex colours, since egui has no gradient primitive.
- **Particle field** — ~110 drifting particles with depth-based parallax and alpha. Positions
  come from a deterministic integer hash rather than `rand`, so screenshots reproduce exactly.
- **Progress rings** — arcs sampled into line segments so they can carry a gradient along their
  length, with a soft glow underneath and a bright cap on the leading edge.
- **Springs** — progress and speed are integrated through a damped spring rather than tweened,
  because the target keeps moving while the animation runs. `dt` is clamped so a stalled frame
  cannot make the integrator explode.
- **Easings** — `out_cubic`, `out_quint`, `out_back`, `out_elastic`, `smoothstep`, used
  deliberately: elastic for the selection tick's pop, quint for card entry, smoothstep for fades.
- **Spectrum** — 84 bars with per-bar spring smoothing and peak markers that decay. It is
  **decorative**: there is no audio decoding or FFT, and the bars are driven by layered sines.
  The API takes a `&[f32]` of magnitudes, so real data could be fed in later.
- **Toasts** — slide in, stack, and dismiss on a timed hairline.
- **Vector icons** — every icon is painted, not a glyph. egui's bundled fonts don't cover `⌕`,
  `✓` or `☾`, which render as tofu boxes; painting them means the UI looks identical everywhere.

Animation is opt-out (Settings → Animations). The app only repaints continuously while something
is actually moving, and idles otherwise, so it doesn't spin the GPU while sitting in the tray.

---

## Differences from the Python version

These are deliberate. Each one is covered by a test.

### `--sync` cannot destroy your library

The Python version deletes every file in the archive it did not see during a run. If a run
enumerates nothing — a 404, a 5xx, rate-limiting, an expired `client_id`, a playlist gone
private — "saw nothing" means "delete everything". It then truncates the archive, so there is
no record of what was lost. That is reproducible: three files and a valid archive, pointed at a
playlist that 404s, leaves an empty directory and a zero-byte archive.

Here:

- A sync plan can only be built from a **successful** enumeration.
- An empty remote listing refuses to delete. `--sync-allow-empty` overrides it.
- Deleting more than half the archive in one run is refused. `--sync-force` overrides it.
- Nothing outside `--path` is ever deleted, whatever the archive says.
- `--dry-run` shows the plan without touching anything.
- The archive is written atomically (temp file + rename), so a crash cannot truncate it.

### Remote metadata cannot escape the download directory

Filename templates are split into path components **before** substitution, so a separator in a
track title, playlist name or uploader is sanitized into a harmless character rather than
creating a directory. A playlist named `..` writes into `_`, not one level up. The result is
re-checked against the download directory before any write.

### Credentials

- `--auth-token` also reads `SCDL_AUTH_TOKEN`, so a token need not appear in `argv` where `ps`
  and shell history capture it.
- The config file is created `0600`. The Python version writes it with the process umask,
  typically leaving an OAuth token world-readable. An existing loose-permission file is warned
  about.
- No credential is ever logged. `--debug` in the Python version prints the token in cleartext
  via three separate paths, and `--debug` is what maintainers ask for in bug reports.
- A config file that fails to parse is an error, not something to silently overwrite with
  defaults — overwriting destroys a stored token.

### Smaller fixes

- **Playlist hydration is chunked by 50.** `GET /tracks?ids=` hard-fails above 50 ids and does
  not preserve request order. yt-dlp sends the whole list unchunked, which 400s on a large
  playlist; results are re-indexed here so playlist track numbers stay correct.
- **Stream resolution falls back through every acceptable format** instead of giving up on the
  first 404, and reports DRM as DRM rather than as an HTTP error.
- **`--add-description` sidecars sit beside their audio file**, inside the playlist folder. The
  Python version writes them all to the base directory, where a custom name format collides
  them into a single appended file.
- **`--extract-artist` ignores a numeric track-number prefix.** A title like `1 - Milky Way`
  tags as `artist=1` upstream; here it falls back to the uploader.
- **Interrupted downloads leave no partial file.** Writes go to `.part` and are renamed on
  success, so `-c` cannot mistake a truncated file for a finished one.

---

## How it works

SoundCloud has no public v2 API and no way to register for a key. The web player authenticates
with a `client_id` baked into its JavaScript, so this does the same: scrape `soundcloud.com`,
walk its `<script src>` tags, and take the first 32-character `client_id`. The id rotates every
few weeks, so a 401/403 triggers one refresh-and-retry.

Audio arrives in three shapes, all handled:

| Shape | What it is | Handling |
|---|---|---|
| `progressive` | a direct URL to the whole file | streamed to disk |
| HLS, no `EXT-X-MAP` | complete MP3 segments | concatenated |
| HLS with `EXT-X-MAP` | fragmented MP4 (AAC) | init segment prepended, then `ffmpeg -c copy` |

Segments are fetched 8-wide and written in playlist order.

---

## Configuration

`~/.config/scdl/scdl.cfg` (`%APPDATA%\scdl\scdl.cfg` on Windows), honouring `XDG_CONFIG_HOME`.
Same format as the original:

```ini
[scdl]
client_id =
auth_token =
path = ~/Music
name_format = [%(id)s] %(uploader)s - %(title)s.%(ext)s
playlist_name_format = %(playlist_index)s. %(uploader)s - %(title)s.%(ext)s
```

Set `path` and you never need `--path` again. A leading `~` is expanded. Name formats accept
both `%(field)s` and the legacy `{field}` syntax, including `{user[username]}` and
`{playlist[title]}`.

---

## Layout

```
crates/scdl-core/     library — no UI, no printing
  client.rs           API client, client_id scraping, pagination
  model.rs            serde models
  stream.rs           transcoding -> format selection -> signed media URL
  download.rs         progressive + HLS, ffmpeg remux
  naming.rs           templating, sanitization, path safety
  tag.rs              metadata and cover art
  archive.rs          download archive + safe sync
  resolve.rs          URL/search -> track list
  pipeline.rs         orchestration, emits progress events
  config.rs           config file
crates/scdl-cli/      the `scdl` binary
  cli.rs, run.rs      argument parsing and the plain CLI path
  tui/                terminal UI
crates/scdl-gui/      the `scdl-gui` binary
  app.rs              views and per-frame layout
  anim.rs             easings, springs, rings, particles, spectrum
  icons.rs            vector icons
  theme.rs            palette and egui style
  state.rs            app state; folds pipeline events
  bridge.rs           async worker <-> UI channel bridge
```

`scdl-core` never prints and has no UI dependencies. All three front-ends consume the same
`Event` stream.

---

## Development

```bash
cargo test
```

178 tests, no network required. The security-relevant ones are worth reading: path traversal via
remote metadata in `naming.rs`, and the sync-deletion guards in `archive.rs`.

```bash
cargo clippy --all-targets && cargo fmt --check
```

### Screenshotting the GUI

The app can capture its own framebuffer, which is more reliable than grabbing an X11 window:

```bash
cargo run --release -p scdl-gui -- --screenshot shot.png --view queue --size 1600x1000
```

Headless, on a machine with no display:

```bash
Xvfb :99 -screen 0 1600x1000x24 & sleep 2 && DISPLAY=:99 LIBGL_ALWAYS_SOFTWARE=1 cargo run --release -p scdl-gui -- --screenshot shot.png --view queue
```

`--screenshot` implies `--demo`, so the frame is seeded with fixed state and needs no network.

Note that frames are **not** pixel-identical between runs: animation is driven by wall-clock time,
so the exact frame captured depends on timing. Screenshots are good for eyeballing a view and for
catching a blank or broken render; they are not suitable for golden-image diffing today. Making
them deterministic would mean pinning egui's clock through `raw_input_hook` and seeding the
particle field from that same clock.

---

## Troubleshooting

**`ffmpeg: not found`** — AAC downloads need it for the remux step. Install it (see Quick start),
or pass `--onlymp3` to stay on a format that needs no remuxing.

**`linker 'cc' not found`** (Linux) — install the build tools line from Quick start.

**GUI fails to start with a GL error** — force software rendering:

```bash
LIBGL_ALWAYS_SOFTWARE=1 scdl-gui
```

**A track fails with "DRM-protected"** — some major-label uploads are encrypted. Nothing to be
done; the CLI reports it rather than writing a broken file.

**A track downloads as 30 seconds** — that is a SoundCloud Go+ preview. Snippets are refused
rather than saved as if they were the full track.

---

## Known limits

- **Go+ tracks** return 30-second previews to unauthenticated clients. A Go+ `auth-token` has
  not been tested.
- **DRM streams** (`ctr-`/`cbc-`/`encrypted-hls`) are detected and reported, not decrypted.
- **429 handling** backs off but has never been triggered in testing, so the exact rate limit is
  unverified.
- The `/me` response shape is inferred; it has not been exercised with a real token.
- **Cancellation is per-track.** The Cancel button stops new tracks from starting and lets
  in-flight ones finish their current file, rather than tearing down mid-write and leaving
  partial files.

---

## Legal

Accesses SoundCloud's internal API in ways its terms of service may not permit. Downloaded
material remains the rights-holders'.

The original scdl is GPL-2.0. This is an independent implementation written against the API
rather than a translation of that code, but it was informed by reading it. GPL obligations
attach on distribution, so a private, undistributed copy triggers none; decide the license
before publishing.
