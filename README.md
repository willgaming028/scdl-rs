# scdl-rs

A SoundCloud downloader in pure Rust, with a terminal UI.

A clean-room reimplementation of [scdl](https://github.com/scdl-org/scdl). It talks to
SoundCloud's v2 API directly — **no Python, no yt-dlp**. One static binary; `ffmpeg` is
needed only to remux AAC streams and for `--flac`.

```
╭ Search ──────────────────────────────────────────────────────────────────────────────╮
│❯ https://soundcloud.com/pandadub/sets/the-lost-ship                                  │
╰──────────────────────────────────────────────────────────────────────────────────────╯
╭ The Lost Ship — 10 tracks — 10/10 selected ────╮┏ Queue — 2 active, 8 done ━━━━━━━━━━┓
│▌◉ 1 - Milky Way 4:27  pandadub                 │┃✓ 1 - Milky Way  pandadub           ┃
│ ◉ 2 - Mayd Hubb Meets Pilgrim - Yabby … 4:50  p│┃   done                             ┃
│ ◉ 3 - Feeling Alive 4:34  pandadub             │┃▼ 8 - Hate  pandadub                ┃
│ ◉ 4 - Lost Reality 4:34  pandadub              │┃ 4.1 MiB  1.2 MiB/s ░░██░░░░░░░░░░░░┃
│ ◉ 5 - Planet Pillow 4:20  pandadub             │┃⠹ 10 - Die Brücke  pandadub         ┃
│ ◉ 6 - Purple trip 6:16  pandadub               │┃   resolving                        ┃
╰────────────────────────────────────────────────╯┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛
╭ Log ─────────────────────────────────────────────────────────────────────────────────╮
│ ✓ pandadub — 5 - Planet Pillow  →  /music/…/5. pandadub - 5 - Planet Pillow.m4a      │
╰──────────────────────────────────────────────────────────────────────────────────────╯
 scdl 0.1.0 80%  Downloading…    j/k move · Space select · Enter download · ? help · q quit
```

## Build

```bash
cargo build --release
```

The binary lands at `target/release/scdl`.

## Use

Run with no arguments for the terminal UI:

```bash
scdl
```

Or use it exactly like the original:

```bash
scdl -l https://soundcloud.com/pandadub/sets/the-lost-ship   # a playlist
scdl -l https://soundcloud.com/quanta-uk -a                  # everything a user posted
scdl -l https://soundcloud.com/kobiblastoyz -f               # a user's likes
scdl -s "aphex twin"                                         # search, take the top hit
scdl me -f                                                   # your own likes
```

Every flag from the original works, plus `-j/--jobs` for parallel downloads, `--tui`/`--no-tui`,
and `--dry-run`. `--yt-dlp-args` is the only one dropped — there is no yt-dlp to forward to.

## Differences from the Python version

These are deliberate. Each one is covered by a test.

### `--sync` cannot destroy your library

The Python version deletes every file in the archive it did not see during a run. If a run
enumerates nothing — a 404, a 5xx, rate-limiting, an expired `client_id`, a playlist gone
private — "saw nothing" means "delete everything". It then truncates the archive, so there is
no record of what was lost. That is reproducible: three files and a valid archive, pointed at
a playlist that 404s, leaves an empty directory and a zero-byte archive.

Here:

- A sync plan can only be built from a **successful** enumeration.
- An empty remote listing refuses to delete. `--sync-allow-empty` overrides it.
- Deleting more than half the archive in one run is refused. `--sync-force` overrides it.
- Nothing outside `--path` is ever deleted, whatever the archive says.
- `--dry-run` shows the plan without touching anything.
- The archive is written atomically (temp file + rename), so a crash cannot truncate it.

### Remote metadata cannot escape the download directory

Filename templates are split into path components **before** substitution, so a separator in
a track title, playlist name or uploader is sanitized into a harmless character rather than
creating a directory. A playlist named `..` writes into `_`, not one level up. The result is
re-checked against the download directory before any write.

### Credentials

- `--auth-token` also reads `SCDL_AUTH_TOKEN`, so a token need not appear in `argv` where
  `ps` and shell history capture it.
- The config file is created `0600`. The Python version writes it with the process umask,
  typically leaving an OAuth token world-readable. An existing loose-permission file is
  warned about.
- No credential is ever logged. `--debug` in the Python version prints the token in cleartext
  via three separate paths, and `--debug` is what maintainers ask for in bug reports.
- A config file that fails to parse is an error, not something to silently overwrite with
  defaults — overwriting destroys a stored token.

### Smaller fixes

- **Playlist hydration is chunked by 50.** `GET /tracks?ids=` hard-fails above 50 ids, and
  does not preserve request order. yt-dlp sends the whole list unchunked, which 400s on a
  large playlist; results are re-indexed here so playlist track numbers stay correct.
- **`--add-description` sidecars sit beside their audio file**, inside the playlist folder.
  The Python version writes them all to the base directory, where a custom name format
  collides them into a single appended file.
- **`--extract-artist` ignores a numeric track-number prefix.** A title like `1 - Milky Way`
  tags as `artist=1` upstream; here it falls back to the uploader.
- **Interrupted downloads leave no partial file.** Writes go to `.part` and are renamed on
  success, so `-c` cannot mistake a truncated file for a finished one.

## How it works

SoundCloud has no public v2 API and no way to register for a key. The web player
authenticates with a `client_id` baked into its JavaScript, so this does the same: scrape
`soundcloud.com`, walk its `<script src>` tags, and take the first 32-character `client_id`.
The id rotates every few weeks, so a 401/403 triggers one refresh-and-retry.

Audio arrives in three shapes, all handled:

| Shape | What it is | Handling |
|---|---|---|
| `progressive` | a direct URL to the whole file | streamed to disk |
| HLS, no `EXT-X-MAP` | complete MP3 segments | concatenated |
| HLS with `EXT-X-MAP` | fragmented MP4 (AAC) | init segment prepended, then `ffmpeg -c copy` |

Segments are fetched 8-wide and written in playlist order.

## Layout

```
crates/scdl-core/     library — no UI, no printing
  client.rs           API client, client_id scraping, pagination
  model.rs            serde models
  stream.rs           transcoding → format selection → signed media URL
  download.rs         progressive + HLS, ffmpeg remux
  naming.rs           templating, sanitization, path safety
  tag.rs              metadata and cover art
  archive.rs          download archive + safe sync
  resolve.rs          URL/search → track list
  pipeline.rs         orchestration, emits progress events
  config.rs           ~/.config/scdl/scdl.cfg
crates/scdl-cli/      binary
  cli.rs              clap definitions
  run.rs              plain CLI path
  tui/                terminal UI
```

`scdl-core` never prints. Both front-ends consume the same `Event` stream.

## Config

`~/.config/scdl/scdl.cfg`, honouring `XDG_CONFIG_HOME`. Same format as the original:

```ini
[scdl]
client_id =
auth_token =
path = .
name_format = [%(id)s] %(uploader)s - %(title)s.%(ext)s
playlist_name_format = %(playlist_index)s. %(uploader)s - %(title)s.%(ext)s
```

Name formats accept both `%(field)s` and the legacy `{field}` syntax, including
`{user[username]}` and `{playlist[title]}`.

## Tests

```bash
cargo test
```

135 tests, no network required. The security-relevant ones are worth reading: path traversal
via remote metadata in `naming.rs`, and the sync-deletion guards in `archive.rs`.

## Known limits

- **Go+ tracks** return 30-second previews to unauthenticated clients; those are refused
  rather than saved as truncated files. A Go+ `auth-token` has not been tested.
- **DRM streams** (`ctr-`/`cbc-`/`encrypted-hls`) are detected and reported, not decrypted.
- **429 handling** backs off but has never been triggered in testing, so the exact rate limit
  is unverified.
- The `/me` response shape is inferred; it has not been exercised with a real token.

## Legal

Accesses SoundCloud's internal API in ways its terms of service may not permit. Downloaded
material remains the rights-holders'.

The original scdl is GPL-2.0. This is an independent implementation written against the API
rather than a translation of that code, but it was informed by reading it. GPL obligations
attach on distribution, so a private, undistributed copy triggers none; decide the license
before publishing.
