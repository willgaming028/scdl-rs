//! Turning a track's `media.transcodings[]` into a concrete, downloadable stream.
//!
//! SoundCloud never hands out a media URL directly. Each transcoding entry holds
//! a *resolver* URL which, when fetched with a valid `client_id` (and the track's
//! `track_authorization` when it has one), returns a short-lived
//! `{"url": "https://..."}` pointing at either a progressive byte stream or an
//! HLS media playlist.

use serde::Deserialize;

use crate::client::Client;
use crate::error::{Error, Result};
use crate::model::{Codec, Format, Protocol, Track, Transcoding};

/// What the caller is willing to accept, assembled from the CLI flags.
#[derive(Debug, Clone, Default)]
pub struct FormatPreferences {
    /// `--onlymp3`: only MP3 streams.
    pub only_mp3: bool,
    /// `--opus`: allow (and prefer) Opus, which is otherwise excluded.
    pub allow_opus: bool,
    /// `--no-original`: never use the uploader's original file.
    pub no_original: bool,
    /// `--only-original`: fail unless the original file is available.
    pub only_original: bool,
}

impl FormatPreferences {
    /// Codec ranking, best first, for the current preferences.
    ///
    /// Mirrors yt-dlp's default order (`aac` > `opus` > `mp3`), except that Opus
    /// is excluded entirely unless `--opus` was passed — matching scdl's
    /// long-standing default of `soundcloud:formats=*_aac,*_mp3`.
    fn codec_rank(&self, codec: Codec) -> Option<u8> {
        match codec {
            Codec::Original => Some(100),
            Codec::Aac if !self.only_mp3 => Some(30),
            Codec::Opus if self.allow_opus && !self.only_mp3 => Some(20),
            Codec::Mp3 => Some(10),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct StreamUrlResponse {
    url: Option<String>,
}

/// A stream that has been resolved to a real, fetchable URL.
#[derive(Debug, Clone)]
pub struct ResolvedStream {
    pub url: String,
    pub format: Format,
}

/// Classify one transcoding entry, or return `None` if it is unusable.
///
/// Returns `Err` only for DRM, which the caller may want to report distinctly
/// from "no matching format".
fn classify(t: &Transcoding) -> std::result::Result<Option<Format>, ()> {
    let url = t.url.as_deref().filter(|u| !u.is_empty()).ok_or(())?;
    let preset = t.preset.as_deref().filter(|p| !p.is_empty()).ok_or(())?;
    let preset_base = preset.split('_').next().unwrap_or(preset);

    let raw_protocol = t
        .format
        .as_ref()
        .and_then(|f| f.protocol.as_deref())
        .unwrap_or("http");

    // `ctr-` / `cbc-` prefixes and `encrypted-hls` are Widevine/FairPlay variants.
    if raw_protocol.starts_with("ctr-")
        || raw_protocol.starts_with("cbc-")
        || raw_protocol == "encrypted-hls"
        || url.contains("/encrypted-hls")
    {
        return Err(());
    }

    let protocol = if raw_protocol == "hls" || url.contains("/hls") {
        Protocol::Hls
    } else {
        Protocol::Http
    };

    // `abr_*` is SoundCloud's adaptive-bitrate manifest. yt-dlp skips it as
    // broken, and it yields a multi-variant playlist we have no use for.
    if preset_base == "abr" {
        return Ok(None);
    }

    let mime = t
        .format
        .as_ref()
        .and_then(|f| f.mime_type.as_deref())
        .unwrap_or("");

    let codec = if preset_base.starts_with("mp3") || mime.contains("mpeg") {
        Codec::Mp3
    } else if preset_base.starts_with("aac") || mime.contains("mp4") {
        Codec::Aac
    } else if preset_base.starts_with("opus") || mime.contains("opus") {
        Codec::Opus
    } else {
        return Ok(None);
    };

    let ext = match codec {
        Codec::Mp3 => "mp3",
        Codec::Aac => "m4a",
        Codec::Opus => "opus",
        Codec::Original => "bin",
    };

    // Bitrate is encoded in presets like `aac_160k`; `hq` implies 256k AAC.
    let is_premium = t.quality.as_deref() == Some("hq");
    let abr = preset
        .rsplit('_')
        .next()
        .and_then(|s| s.strip_suffix('k'))
        .and_then(|s| s.parse::<u32>().ok())
        .or(if is_premium && codec == Codec::Aac {
            Some(256)
        } else {
            None
        });

    Ok(Some(Format {
        transcoding_url: url.to_string(),
        protocol,
        codec,
        preset: preset.to_string(),
        ext,
        abr,
        is_preview: t.snipped || url.contains("/preview/"),
        is_premium,
    }))
}

/// True when the track offers DRM-protected transcodings.
///
/// Worth checking separately from [`available_formats`]: a track can have
/// usable-looking non-DRM entries that all fail to resolve, in which case DRM is
/// the real explanation and an HTTP 404 is just the symptom.
pub fn has_drm_transcodings(track: &Track) -> bool {
    track
        .media
        .as_ref()
        .map(|m| m.transcodings.iter().any(|t| classify(t).is_err()))
        .unwrap_or(false)
}

/// All usable formats for a track, best first.
pub fn available_formats(track: &Track, prefs: &FormatPreferences) -> Result<Vec<Format>> {
    let Some(media) = track.media.as_ref() else {
        return Err(Error::NoFormat {
            title: track.title_or_untitled().to_string(),
        });
    };

    let mut saw_drm = false;
    let mut formats: Vec<Format> = Vec::new();

    for t in &media.transcodings {
        match classify(t) {
            Err(()) => saw_drm = true,
            Ok(None) => {}
            Ok(Some(f)) => {
                if prefs.codec_rank(f.codec).is_some() {
                    formats.push(f);
                }
            }
        }
    }

    if formats.is_empty() {
        if saw_drm {
            return Err(Error::Drm {
                title: track.title_or_untitled().to_string(),
            });
        }
        return Err(Error::NoFormat {
            title: track.title_or_untitled().to_string(),
        });
    }

    // Best first: non-preview, then codec preference, then quality/bitrate, then
    // progressive over HLS (one request beats N segment requests for the same bytes).
    formats.sort_by(|a, b| {
        let key = |f: &Format| {
            (
                !f.is_preview,
                prefs.codec_rank(f.codec).unwrap_or(0),
                f.rank(),
                f.protocol == Protocol::Http,
            )
        };
        key(b).cmp(&key(a))
    });

    Ok(formats)
}

/// Pick the single best format for a track.
pub fn best_format(track: &Track, prefs: &FormatPreferences) -> Result<Format> {
    let formats = available_formats(track, prefs)?;
    let best = formats.into_iter().next().ok_or_else(|| Error::NoFormat {
        title: track.title_or_untitled().to_string(),
    })?;

    if best.is_preview {
        return Err(Error::Snipped {
            title: track.title_or_untitled().to_string(),
        });
    }
    Ok(best)
}

/// Exchange a transcoding's resolver URL for a real media URL.
///
/// The returned URL is short-lived (minutes), so resolve immediately before
/// downloading rather than up front for a whole queue.
pub async fn resolve_stream_url(
    client: &Client,
    track: &Track,
    format: &Format,
) -> Result<ResolvedStream> {
    let mut query: Vec<(&str, &str)> = Vec::new();
    if let Some(auth) = track.track_authorization.as_deref() {
        query.push(("track_authorization", auth));
    }
    if let Some(secret) = track.secret_token.as_deref() {
        query.push(("secret_token", secret));
    }

    let resp: StreamUrlResponse = client.call_api(&format.transcoding_url, &query).await?;

    let url = resp.url.filter(|u| !u.is_empty()).ok_or(Error::NoFormat {
        title: track.title_or_untitled().to_string(),
    })?;

    Ok(ResolvedStream {
        url,
        format: format.clone(),
    })
}

/// The uploader's original, untranscoded file, when they have allowed downloads.
///
/// Returns `Ok(None)` when the track simply is not downloadable, and an error
/// only when something actually went wrong.
pub async fn original_download(client: &Client, track: &Track) -> Result<Option<OriginalDownload>> {
    if !(track.downloadable && track.has_downloads_left) {
        return Ok(None);
    }

    #[derive(Deserialize)]
    struct DownloadResponse {
        #[serde(rename = "redirectUri")]
        redirect_uri: Option<String>,
    }

    let resp: DownloadResponse = match client
        .call_api(
            &format!("{}tracks/{}/download", crate::client::API_V2, track.id),
            &[],
        )
        .await
    {
        Ok(r) => r,
        // 401 = needs a logged-in account; 403 = not permitted for this client.
        // Neither is fatal: fall back to a transcoded stream.
        Err(Error::Http { status: 401, .. }) | Err(Error::Http { status: 403, .. }) => {
            return Ok(None)
        }
        Err(e) => return Err(e),
    };

    let Some(uri) = resp.redirect_uri.filter(|u| !u.is_empty()) else {
        return Ok(None);
    };

    Ok(Some(OriginalDownload { url: uri }))
}

#[derive(Debug, Clone)]
pub struct OriginalDownload {
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Media, TranscodingFormat};

    fn tc(preset: &str, protocol: &str, mime: &str) -> Transcoding {
        Transcoding {
            url: Some(format!("https://api-v2.soundcloud.com/media/{preset}")),
            preset: Some(preset.to_string()),
            format: Some(TranscodingFormat {
                protocol: Some(protocol.to_string()),
                mime_type: Some(mime.to_string()),
            }),
            ..Transcoding::default()
        }
    }

    /// The exact transcoding set the live API returned for a real track during
    /// development, so the selector is tested against reality rather than a guess.
    fn real_track() -> Track {
        let mut t: Track = serde_json::from_str(r#"{"id": 200924286, "title": "1 - Milky Way"}"#)
            .expect("fixture");
        t.media = Some(Media {
            transcodings: vec![
                tc("aac_160k", "hls", r#"audio/mp4; codecs="mp4a.40.2""#),
                tc("aac_96k", "hls", r#"audio/mp4; codecs="mp4a.40.2""#),
                tc("abr_sq", "hls", "audio/mpeg"),
                tc("mp3_0_0", "hls", "audio/mpeg"),
                tc("mp3_0_0", "progressive", "audio/mpeg"),
            ],
        });
        t
    }

    #[test]
    fn default_prefs_pick_aac_and_drop_abr() {
        let t = real_track();
        let fs = available_formats(&t, &FormatPreferences::default()).unwrap();
        assert!(
            !fs.iter().any(|f| f.preset.starts_with("abr")),
            "abr must be skipped"
        );
        assert_eq!(fs[0].codec, Codec::Aac);
        assert_eq!(fs[0].abr, Some(160));
        assert_eq!(fs[0].ext, "m4a");
    }

    #[test]
    fn only_mp3_excludes_aac_and_prefers_progressive() {
        let t = real_track();
        let prefs = FormatPreferences {
            only_mp3: true,
            ..Default::default()
        };
        let fs = available_formats(&t, &prefs).unwrap();
        assert!(fs.iter().all(|f| f.codec == Codec::Mp3));
        // Same codec and bitrate on both, so progressive should win the tiebreak.
        assert_eq!(fs[0].protocol, Protocol::Http);
    }

    #[test]
    fn opus_is_excluded_unless_requested() {
        let mut t = real_track();
        t.media.as_mut().unwrap().transcodings.push(tc(
            "opus_0_0",
            "hls",
            r#"audio/ogg; codecs="opus""#,
        ));

        let without = available_formats(&t, &FormatPreferences::default()).unwrap();
        assert!(without.iter().all(|f| f.codec != Codec::Opus));

        let with = available_formats(
            &t,
            &FormatPreferences {
                allow_opus: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(with.iter().any(|f| f.codec == Codec::Opus));
    }

    #[test]
    fn drm_transcodings_are_reported_as_drm_not_missing() {
        let mut t = real_track();
        t.media.as_mut().unwrap().transcodings =
            vec![tc("aac_160k", "ctr-encrypted-hls", "audio/mp4")];
        match available_formats(&t, &FormatPreferences::default()) {
            Err(Error::Drm { .. }) => {}
            other => panic!("expected Drm, got {other:?}"),
        }
    }

    #[test]
    fn hls_detected_from_url_even_when_protocol_lies() {
        let mut t = tc("mp3_0_0", "progressive", "audio/mpeg");
        t.url = Some("https://api-v2.soundcloud.com/media/hls/xyz".into());
        let f = classify(&t).unwrap().unwrap();
        assert_eq!(f.protocol, Protocol::Hls);
    }

    #[test]
    fn premium_hq_aac_infers_256k() {
        let mut t = tc("aac_hq", "hls", "audio/mp4");
        t.quality = Some("hq".into());
        let f = classify(&t).unwrap().unwrap();
        assert!(f.is_premium);
        assert_eq!(f.abr, Some(256));
    }

    #[test]
    fn snipped_track_is_rejected_by_best_format() {
        let mut t = real_track();
        for tc in &mut t.media.as_mut().unwrap().transcodings {
            tc.snipped = true;
        }
        match best_format(&t, &FormatPreferences::default()) {
            Err(Error::Snipped { .. }) => {}
            other => panic!("expected Snipped, got {other:?}"),
        }
    }

    #[test]
    fn short_id_matches_ytdlp_convention() {
        let f = classify(&tc("aac_160k", "hls", "audio/mp4"))
            .unwrap()
            .unwrap();
        assert_eq!(f.short_id(), "hls_aac");
    }
}

#[cfg(test)]
mod drm_tests {
    use super::*;
    use crate::model::{Media, TranscodingFormat};

    fn tc(preset: &str, protocol: &str) -> Transcoding {
        Transcoding {
            url: Some("https://api-v2.soundcloud.com/media/x".into()),
            preset: Some(preset.into()),
            format: Some(TranscodingFormat {
                protocol: Some(protocol.into()),
                mime_type: Some("audio/mp4".into()),
            }),
            ..Transcoding::default()
        }
    }

    /// Shape observed live on a rights-managed major-label upload: AAC behind
    /// Widevine/FairPlay, with MP3 entries that resolve to 404.
    #[test]
    fn detects_drm_alongside_usable_looking_formats() {
        let mut t: Track = serde_json::from_str(r#"{"id": 1, "title": "Roygbiv"}"#).unwrap();
        t.media = Some(Media {
            transcodings: vec![
                tc("aac_160k", "cbc-encrypted-hls"),
                tc("aac_160k", "ctr-encrypted-hls"),
                tc("aac_96k", "cbc-encrypted-hls"),
                tc("abr_sq", "ctr-encrypted-hls"),
                tc("mp3_1_0", "hls"),
                tc("mp3_1_0", "progressive"),
            ],
        });

        assert!(has_drm_transcodings(&t));
        // The MP3 entries still look usable, so format selection succeeds...
        let fs = available_formats(&t, &FormatPreferences::default()).unwrap();
        assert_eq!(fs.len(), 2);
        assert!(fs.iter().all(|f| f.codec == Codec::Mp3));
        // ...which is exactly why DRM has to be reported separately when they
        // all fail to resolve.
    }

    #[test]
    fn no_drm_on_an_ordinary_track() {
        let mut t: Track = serde_json::from_str(r#"{"id": 1}"#).unwrap();
        t.media = Some(Media {
            transcodings: vec![tc("mp3_0_0", "progressive"), tc("aac_160k", "hls")],
        });
        assert!(!has_drm_transcodings(&t));
    }
}
