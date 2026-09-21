//! Fetching bytes: progressive streams, HLS assembly, and ffmpeg remuxing.
//!
//! Three shapes show up in practice, all confirmed against the live API:
//!
//! * **progressive** — the resolver hands back a direct URL to the whole file.
//! * **HLS without `EXT-X-MAP`** (SoundCloud's MP3 HLS) — a list of complete MP3
//!   segments that concatenate into a valid file with no further processing.
//! * **HLS with `EXT-X-MAP`** (SoundCloud's AAC) — fragmented MP4. The init
//!   segment must be written first, and the result needs an ffmpeg remux before
//!   most players and taggers will accept it as an `.m4a`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

use crate::client::Client;
use crate::error::{Error, Result};
use crate::model::Protocol;
use crate::stream::ResolvedStream;

/// How many HLS segments to fetch at once. SoundCloud's CDN is happy to serve in
/// parallel and segments are ~10s each, so this is the difference between a
/// track taking 2 seconds and 30.
const SEGMENT_CONCURRENCY: usize = 8;

/// Progress callback payload.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub downloaded: u64,
    /// `None` when the server did not send a length and we cannot infer one.
    pub total: Option<u64>,
}

impl Progress {
    pub fn fraction(&self) -> Option<f64> {
        self.total.filter(|t| *t > 0).map(|t| {
            let f = self.downloaded as f64 / t as f64;
            f.clamp(0.0, 1.0)
        })
    }
}

/// Anything that wants to watch bytes arrive.
pub trait ProgressSink: Send + Sync {
    fn on_progress(&self, p: Progress);
}

impl<F> ProgressSink for F
where
    F: Fn(Progress) + Send + Sync,
{
    fn on_progress(&self, p: Progress) {
        self(p)
    }
}

/// A sink that discards everything, for callers that do not care.
pub struct NoProgress;
impl ProgressSink for NoProgress {
    fn on_progress(&self, _: Progress) {}
}

/// Result of a completed download, before tagging.
#[derive(Debug, Clone)]
pub struct DownloadedFile {
    pub path: PathBuf,
    pub bytes: u64,
    /// True when the bytes went through ffmpeg rather than straight to disk.
    pub remuxed: bool,
}

/// Download a resolved stream to `dest`.
///
/// Writes to a sibling `.part` file and renames on success, so an interrupted
/// run never leaves a truncated file that a later `-c` run would mistake for a
/// completed download.
pub async fn download_stream(
    client: &Client,
    stream: &ResolvedStream,
    dest: &Path,
    sink: Arc<dyn ProgressSink>,
) -> Result<DownloadedFile> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| Error::io(parent, e))?;
    }

    let part = part_path(dest);

    let outcome = match stream.format.protocol {
        Protocol::Http => download_progressive(client, &stream.url, &part, sink).await,
        Protocol::Hls => download_hls(client, &stream.url, &part, sink).await,
    };

    // Never leave a .part behind on failure — it would confuse the next run.
    let hls = match outcome {
        Ok(v) => v,
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(e);
        }
    };

    // fMP4 fragments need a remux before anything will treat the file as an m4a.
    let remuxed = hls.needs_remux;
    let final_bytes = if remuxed {
        let remuxed_part = part.with_extension("remux.part");
        remux(&part, &remuxed_part, "mp4").await?;
        let _ = tokio::fs::remove_file(&part).await;
        let size = file_len(&remuxed_part).await?;
        rename(&remuxed_part, dest).await?;
        size
    } else {
        let size = file_len(&part).await?;
        rename(&part, dest).await?;
        size
    };

    Ok(DownloadedFile {
        path: dest.to_path_buf(),
        bytes: final_bytes,
        remuxed,
    })
}

struct HlsOutcome {
    needs_remux: bool,
}

async fn download_progressive(
    client: &Client,
    url: &str,
    part: &Path,
    sink: Arc<dyn ProgressSink>,
) -> Result<HlsOutcome> {
    let resp = client.http().get(url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(Error::Http {
            status: status.as_u16(),
            url: url.to_string(),
        });
    }

    let total = resp.content_length();
    let mut file = tokio::fs::File::create(part)
        .await
        .map_err(|e| Error::io(part, e))?;

    let mut downloaded: u64 = 0;
    let mut body = resp.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        file.write_all(&chunk)
            .await
            .map_err(|e| Error::io(part, e))?;
        downloaded += chunk.len() as u64;
        sink.on_progress(Progress { downloaded, total });
    }
    file.flush().await.map_err(|e| Error::io(part, e))?;

    Ok(HlsOutcome { needs_remux: false })
}

async fn download_hls(
    client: &Client,
    playlist_url: &str,
    part: &Path,
    sink: Arc<dyn ProgressSink>,
) -> Result<HlsOutcome> {
    let text = client
        .http()
        .get(playlist_url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    let mut parsed = parse_media_playlist(&text)?;

    if parsed.encrypted {
        // We have never seen SoundCloud serve EXT-X-KEY, but failing loudly beats
        // silently writing a file full of ciphertext.
        return Err(Error::NoFormat {
            title: "encrypted HLS stream is not supported".to_string(),
        });
    }

    let mut file = tokio::fs::File::create(part)
        .await
        .map_err(|e| Error::io(part, e))?;

    let downloaded = Arc::new(AtomicU64::new(0));

    // The init segment must land first, and alone, before any media segment.
    if let Some(init) = &parsed.init_segment {
        let bytes = fetch_bytes(client, init).await?;
        downloaded.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        sink.on_progress(Progress {
            downloaded: downloaded.load(Ordering::Relaxed),
            total: None,
        });
        file.write_all(&bytes)
            .await
            .map_err(|e| Error::io(part, e))?;
    }

    // Fetch ahead in parallel but write strictly in playlist order.
    // Iterate owned Strings rather than `&String`: a closure taking a reference
    // and returning an async block cannot satisfy the higher-ranked bound that
    // `tokio::spawn` ultimately requires of the enclosing future.
    let segments = std::mem::take(&mut parsed.segments);
    let mut ordered = futures_util::stream::iter(segments.into_iter().map(|url| {
        let client = client.clone();
        let downloaded = Arc::clone(&downloaded);
        let sink = Arc::clone(&sink);
        async move {
            let bytes = fetch_bytes(&client, &url).await?;
            let n =
                downloaded.fetch_add(bytes.len() as u64, Ordering::Relaxed) + bytes.len() as u64;
            sink.on_progress(Progress {
                downloaded: n,
                total: None,
            });
            Ok::<_, Error>(bytes)
        }
    }))
    .buffered(SEGMENT_CONCURRENCY);

    while let Some(chunk) = ordered.next().await {
        let chunk = chunk?;
        file.write_all(&chunk)
            .await
            .map_err(|e| Error::io(part, e))?;
    }
    file.flush().await.map_err(|e| Error::io(part, e))?;

    Ok(HlsOutcome {
        // Concatenated fMP4 fragments are not a well-formed MP4 until remuxed.
        needs_remux: parsed.init_segment.is_some(),
    })
}

async fn fetch_bytes(client: &Client, url: &str) -> Result<bytes_compat::Bytes> {
    let resp = client.http().get(url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(Error::Http {
            status: status.as_u16(),
            url: url.to_string(),
        });
    }
    Ok(resp.bytes().await?)
}

/// Keeps the `bytes` type name local so a reqwest bump cannot ripple outward.
mod bytes_compat {
    pub type Bytes = ::bytes::Bytes;
}

#[derive(Debug, Default)]
struct MediaPlaylist {
    init_segment: Option<String>,
    segments: Vec<String>,
    encrypted: bool,
}

/// Parse the subset of HLS that SoundCloud actually emits.
///
/// Hand-rolled rather than pulled from a crate because the grammar in play here
/// is four tags wide, and the failure mode we care about (an `EXT-X-KEY` we do
/// not understand) is one we want to detect explicitly.
fn parse_media_playlist(text: &str) -> Result<MediaPlaylist> {
    let mut out = MediaPlaylist::default();

    if !text.trim_start().starts_with("#EXTM3U") {
        return Err(Error::NoFormat {
            title: "stream URL did not return an HLS playlist".to_string(),
        });
    }

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            if let Some(uri) = attribute(rest, "URI") {
                out.init_segment = Some(uri);
            }
        } else if line.starts_with("#EXT-X-KEY:") {
            // METHOD=NONE is a no-op and does not mean encrypted.
            if attribute(line, "METHOD").as_deref() != Some("NONE") {
                out.encrypted = true;
            }
        } else if !line.starts_with('#') {
            out.segments.push(line.to_string());
        }
    }

    if out.segments.is_empty() {
        return Err(Error::NoFormat {
            title: "HLS playlist contained no segments".to_string(),
        });
    }

    Ok(out)
}

/// Pull `KEY="value"` or `KEY=value` out of an HLS attribute list.
fn attribute(s: &str, key: &str) -> Option<String> {
    let idx = s.find(&format!("{key}="))?;
    let rest = &s[idx + key.len() + 1..];
    if let Some(stripped) = rest.strip_prefix('"') {
        stripped.find('"').map(|end| stripped[..end].to_string())
    } else {
        let end = rest.find(',').unwrap_or(rest.len());
        Some(rest[..end].to_string())
    }
}

/// Remux with ffmpeg, copying streams rather than re-encoding.
pub async fn remux(input: &Path, output: &Path, format: &str) -> Result<()> {
    run_ffmpeg(&[
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-i",
        &input.to_string_lossy(),
        "-c",
        "copy",
        "-f",
        format,
        &output.to_string_lossy(),
    ])
    .await
}

/// Re-encode to FLAC. Only meaningful for lossless sources (`--flac`).
pub async fn recode_flac(input: &Path, output: &Path) -> Result<()> {
    run_ffmpeg(&[
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-i",
        &input.to_string_lossy(),
        "-c:a",
        "flac",
        &output.to_string_lossy(),
    ])
    .await
}

async fn run_ffmpeg(args: &[&str]) -> Result<()> {
    let out = tokio::process::Command::new("ffmpeg")
        .args(args)
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::FfmpegMissing("a remuxed audio file".to_string())
            } else {
                Error::PlainIo(e)
            }
        })?;

    if !out.status.success() {
        return Err(Error::Ffmpeg {
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr)
                .lines()
                .take(5)
                .collect::<Vec<_>>()
                .join("; "),
        });
    }
    Ok(())
}

pub fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

async fn file_len(p: &Path) -> Result<u64> {
    Ok(tokio::fs::metadata(p)
        .await
        .map_err(|e| Error::io(p, e))?
        .len())
}

async fn rename(from: &Path, to: &Path) -> Result<()> {
    match tokio::fs::rename(from, to).await {
        Ok(()) => Ok(()),
        // Cross-device rename fails; fall back to copy + delete.
        Err(_) => {
            tokio::fs::copy(from, to)
                .await
                .map_err(|e| Error::io(to, e))?;
            let _ = tokio::fs::remove_file(from).await;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shape of SoundCloud's AAC playlist, as captured from the live API.
    const AAC_PLAYLIST: &str = r#"#EXTM3U
#EXT-X-VERSION:7
#EXT-X-TARGETDURATION:10
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-MAP:URI="https://playback.example/aac_160k/init.mp4"
#EXTINF:10.007800,
https://playback.example/aac_160k/seg0.m4s
#EXTINF:10.007800,
https://playback.example/aac_160k/seg1.m4s
#EXT-X-ENDLIST
"#;

    /// Shape of SoundCloud's MP3 playlist: no init segment.
    const MP3_PLAYLIST: &str = r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-TARGETDURATION:10
#EXT-X-MEDIA-SEQUENCE:0
#EXTINF:1.985272,
https://cf-hls-media.sndcdn.com/media/a.128.mp3?Policy=abc
#EXTINF:2.977908,
https://cf-hls-media.sndcdn.com/media/b.128.mp3?Policy=abc
#EXT-X-ENDLIST
"#;

    #[test]
    fn parses_fmp4_playlist_with_init_segment() {
        let p = parse_media_playlist(AAC_PLAYLIST).unwrap();
        assert_eq!(
            p.init_segment.as_deref(),
            Some("https://playback.example/aac_160k/init.mp4")
        );
        assert_eq!(p.segments.len(), 2);
        assert!(!p.encrypted);
    }

    #[test]
    fn parses_mp3_playlist_without_init_segment() {
        let p = parse_media_playlist(MP3_PLAYLIST).unwrap();
        assert!(p.init_segment.is_none());
        assert_eq!(p.segments.len(), 2);
        // Query strings must survive: the CDN policy signature lives there.
        assert!(p.segments[0].contains("Policy=abc"));
    }

    #[test]
    fn only_fmp4_playlists_request_a_remux() {
        assert!(parse_media_playlist(AAC_PLAYLIST)
            .unwrap()
            .init_segment
            .is_some());
        assert!(parse_media_playlist(MP3_PLAYLIST)
            .unwrap()
            .init_segment
            .is_none());
    }

    #[test]
    fn detects_encryption_and_ignores_method_none() {
        let enc = AAC_PLAYLIST.replace(
            "#EXT-X-VERSION:7",
            "#EXT-X-VERSION:7\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"",
        );
        assert!(parse_media_playlist(&enc).unwrap().encrypted);

        let none = AAC_PLAYLIST.replace(
            "#EXT-X-VERSION:7",
            "#EXT-X-VERSION:7\n#EXT-X-KEY:METHOD=NONE",
        );
        assert!(!parse_media_playlist(&none).unwrap().encrypted);
    }

    #[test]
    fn rejects_non_playlist_bodies() {
        assert!(parse_media_playlist("<html>nope</html>").is_err());
        assert!(parse_media_playlist("#EXTM3U\n#EXT-X-ENDLIST").is_err());
    }

    #[test]
    fn attribute_parsing_handles_quotes_and_bare_values() {
        assert_eq!(
            attribute(r#"URI="https://a/b.mp4",BYTERANGE="1@0""#, "URI").as_deref(),
            Some("https://a/b.mp4")
        );
        assert_eq!(
            attribute("METHOD=NONE,FOO=bar", "METHOD").as_deref(),
            Some("NONE")
        );
    }

    #[test]
    fn part_path_appends_rather_than_replacing_extension() {
        // `.with_extension` would turn "a.b.mp3" into "a.b.part" and lose data.
        assert_eq!(
            part_path(Path::new("/tmp/My Track feat. X.mp3")),
            PathBuf::from("/tmp/My Track feat. X.mp3.part")
        );
    }

    #[test]
    fn progress_fraction_is_clamped_and_safe() {
        assert_eq!(
            Progress {
                downloaded: 50,
                total: Some(100)
            }
            .fraction(),
            Some(0.5)
        );
        assert_eq!(
            Progress {
                downloaded: 5,
                total: Some(0)
            }
            .fraction(),
            None
        );
        assert_eq!(
            Progress {
                downloaded: 500,
                total: Some(100)
            }
            .fraction(),
            Some(1.0)
        );
    }
}
