//! Serde models for the SoundCloud v2 API.
//!
//! Field names and shapes are taken from what yt-dlp's SoundCloud extractor
//! actually reads, so anything here has been observed in real responses. Every
//! field is optional unless the API is genuinely guaranteed to return it, because
//! SoundCloud freely omits keys depending on the endpoint, on whether the caller
//! is authenticated, and on the track's privacy settings.

use serde::{Deserialize, Serialize};

/// Minimal user object as embedded in tracks and playlists.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct User {
    pub id: Option<i64>,
    pub username: Option<String>,
    pub permalink: Option<String>,
    pub permalink_url: Option<String>,
    pub avatar_url: Option<String>,
    pub full_name: Option<String>,
    pub verified: Option<bool>,
    pub followers_count: Option<i64>,
    pub track_count: Option<i64>,
}

impl User {
    pub fn display_name(&self) -> &str {
        self.username
            .as_deref()
            .or(self.permalink.as_deref())
            .unwrap_or("unknown artist")
    }
}

/// `publisher_metadata` carries the rights-holder's own idea of artist/album,
/// which is usually better than the uploader's username when present.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PublisherMetadata {
    pub id: Option<i64>,
    pub artist: Option<String>,
    pub album_title: Option<String>,
    pub isrc: Option<String>,
    pub explicit: Option<bool>,
    pub publisher: Option<String>,
    pub writer_composer: Option<String>,
    pub release_title: Option<String>,
    pub upc_or_ean: Option<String>,
    pub p_line: Option<String>,
    pub c_line: Option<String>,
}

/// The `format` sub-object of a transcoding.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TranscodingFormat {
    /// `"progressive"`, `"hls"`, `"encrypted-hls"`, or a `ctr-`/`cbc-` DRM scheme.
    pub protocol: Option<String>,
    /// e.g. `audio/mpeg`, `audio/mp4; codecs="mp4a.40.2"`, `audio/ogg; codecs="opus"`.
    pub mime_type: Option<String>,
}

/// One entry of `media.transcodings[]`. The `url` here is *not* a media URL — it
/// must itself be fetched (with `client_id`, and `track_authorization` when the
/// track carries one) to obtain a short-lived JSON `{"url": "..."}` pointing at
/// the real stream.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Transcoding {
    pub url: Option<String>,
    /// e.g. `mp3_0_0`, `mp3_1_0`, `opus_0_0`, `aac_160k`, `aac_256k`, `abr_sq`.
    pub preset: Option<String>,
    pub duration: Option<i64>,
    /// True for 30-second Go+ previews.
    #[serde(default)]
    pub snipped: bool,
    pub format: Option<TranscodingFormat>,
    /// `"sq"` or `"hq"`; `hq` indicates a Go+ high-quality stream.
    pub quality: Option<String>,
    pub is_legacy_transcoding: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Media {
    #[serde(default)]
    pub transcodings: Vec<Transcoding>,
}

/// A SoundCloud track.
///
/// `id` is the only field we insist on: a response without one is not a track.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Track {
    pub id: i64,
    pub title: Option<String>,
    pub permalink: Option<String>,
    pub permalink_url: Option<String>,
    pub description: Option<String>,
    /// Milliseconds.
    pub duration: Option<i64>,
    /// Milliseconds; full length even when only a snippet is streamable.
    pub full_duration: Option<i64>,
    pub genre: Option<String>,
    /// Space-separated, with quoted multi-word tags.
    pub tag_list: Option<String>,
    pub license: Option<String>,
    /// RFC 3339, e.g. `2019-01-02T03:04:05Z`.
    pub created_at: Option<String>,
    pub display_date: Option<String>,
    pub last_modified: Option<String>,
    pub release_date: Option<String>,
    pub artwork_url: Option<String>,
    pub user: Option<User>,
    pub user_id: Option<i64>,
    pub media: Option<Media>,
    /// Required as a query parameter when resolving transcoding URLs for some tracks.
    pub track_authorization: Option<String>,
    #[serde(default)]
    pub downloadable: bool,
    /// Note the exact spelling: `has_downloads_left`, not `has_downloadable_left`.
    #[serde(default)]
    pub has_downloads_left: bool,
    pub streamable: Option<bool>,
    pub playback_count: Option<i64>,
    pub likes_count: Option<i64>,
    pub favoritings_count: Option<i64>,
    pub comment_count: Option<i64>,
    pub reposts_count: Option<i64>,
    pub download_count: Option<i64>,
    pub label_name: Option<String>,
    pub publisher_metadata: Option<PublisherMetadata>,
    /// `"ALLOW"`, `"BLOCK"`, `"MONETIZE"`, `"SNIP"`.
    pub policy: Option<String>,
    pub monetization_model: Option<String>,
    pub sharing: Option<String>,
    pub secret_token: Option<String>,
    pub kind: Option<String>,
    pub purchase_url: Option<String>,
    pub purchase_title: Option<String>,

    /// Not from the API: the playlist context this track was reached through, if
    /// any. Populated by the resolver so templating and album tagging can see it.
    #[serde(skip)]
    pub playlist_context: Option<PlaylistContext>,
}

impl Track {
    pub fn title_or_untitled(&self) -> &str {
        self.title.as_deref().unwrap_or("Untitled")
    }

    pub fn artist(&self) -> &str {
        self.publisher_metadata
            .as_ref()
            .and_then(|p| p.artist.as_deref())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.user.as_ref().map(|u| u.display_name()))
            .unwrap_or("unknown artist")
    }

    /// The uploader's username, ignoring `publisher_metadata`.
    pub fn uploader(&self) -> &str {
        self.user
            .as_ref()
            .map(|u| u.display_name())
            .unwrap_or("unknown artist")
    }

    pub fn duration_secs(&self) -> Option<f64> {
        self.duration.map(|ms| ms as f64 / 1000.0)
    }

    /// True when SoundCloud will only serve a 30-second snippet.
    pub fn is_snipped(&self) -> bool {
        self.policy.as_deref() == Some("SNIP")
            || self
                .media
                .as_ref()
                .map(|m| !m.transcodings.is_empty() && m.transcodings.iter().all(|t| t.snipped))
                .unwrap_or(false)
    }

    pub fn is_geo_blocked(&self) -> bool {
        self.policy.as_deref() == Some("BLOCK")
    }

    /// Tags parsed out of `tag_list`, honouring the quoted-multi-word convention.
    pub fn tags(&self) -> Vec<String> {
        let Some(raw) = self.tag_list.as_deref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut chars = raw.chars().peekable();
        let mut current = String::new();
        let mut in_quotes = false;
        while let Some(c) = chars.next() {
            match c {
                '"' => {
                    in_quotes = !in_quotes;
                    if !in_quotes && !current.is_empty() {
                        out.push(std::mem::take(&mut current));
                    }
                }
                ' ' if !in_quotes => {
                    if !current.is_empty() {
                        out.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(c),
            }
            let _ = chars.peek();
        }
        if !current.is_empty() {
            out.push(current);
        }
        out
    }
}

/// Where a track sits inside a playlist, for album tags and `%(playlist_index)s`.
#[derive(Debug, Clone)]
pub struct PlaylistContext {
    pub id: i64,
    pub title: String,
    pub uploader: String,
    /// 1-based.
    pub index: usize,
    pub total: usize,
}

/// A playlist / set / album.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Playlist {
    pub id: i64,
    pub title: Option<String>,
    pub permalink: Option<String>,
    pub permalink_url: Option<String>,
    pub description: Option<String>,
    pub duration: Option<i64>,
    pub genre: Option<String>,
    pub tag_list: Option<String>,
    pub license: Option<String>,
    pub created_at: Option<String>,
    pub last_modified: Option<String>,
    pub release_date: Option<String>,
    pub published_at: Option<String>,
    pub artwork_url: Option<String>,
    pub user: Option<User>,
    pub track_count: Option<i64>,
    pub likes_count: Option<i64>,
    pub reposts_count: Option<i64>,
    /// `"album"`, `"ep"`, `"single"`, `"compilation"`, or empty for a plain set.
    pub set_type: Option<String>,
    pub is_album: Option<bool>,
    pub secret_token: Option<String>,
    pub sharing: Option<String>,
    pub kind: Option<String>,
    /// Frequently a mix of full track objects and `{"id": N}` stubs that must be
    /// hydrated through `GET /tracks?ids=...`.
    #[serde(default)]
    pub tracks: Vec<Track>,
}

impl Playlist {
    pub fn title_or_untitled(&self) -> &str {
        self.title.as_deref().unwrap_or("Untitled Playlist")
    }

    pub fn uploader(&self) -> &str {
        self.user
            .as_ref()
            .map(|u| u.display_name())
            .unwrap_or("unknown artist")
    }
}

/// A `linked_partitioning` page.
#[derive(Debug, Clone, Deserialize)]
pub struct Page<T> {
    #[serde(default = "Vec::new")]
    pub collection: Vec<T>,
    pub next_href: Option<String>,
    pub total_results: Option<i64>,
}

/// Stream-selection candidate derived from a [`Transcoding`], after we have
/// classified its protocol and codec but before the media URL is resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    pub transcoding_url: String,
    pub protocol: Protocol,
    pub codec: Codec,
    pub preset: String,
    /// File extension the finished file should have.
    pub ext: &'static str,
    /// Approximate average bitrate in kbps, where known.
    pub abr: Option<u32>,
    pub is_preview: bool,
    pub is_premium: bool,
}

impl Format {
    /// yt-dlp-style short identifier, e.g. `http_mp3`, `hls_aac`.
    pub fn short_id(&self) -> String {
        let base = self.preset.split('_').next().unwrap_or(&self.preset);
        format!("{}_{}", self.protocol.as_str(), base)
    }

    /// Ordering key for "best audio". Higher is better.
    pub fn rank(&self) -> (i32, i32, u32) {
        let preview_penalty = if self.is_preview { -10 } else { 0 };
        let quality = if self.is_premium {
            5
        } else if self.abr.is_some_and(|a| a >= 160) {
            0
        } else {
            -1
        };
        (preview_penalty, quality, self.abr.unwrap_or(0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// A plain HTTP byte stream (SoundCloud calls it `progressive`).
    Http,
    Hls,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Http => "http",
            Protocol::Hls => "hls",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Mp3,
    Aac,
    Opus,
    /// The untranscoded file the artist uploaded, whatever it happens to be.
    Original,
}

impl Codec {
    pub fn as_str(self) -> &'static str {
        match self {
            Codec::Mp3 => "mp3",
            Codec::Aac => "aac",
            Codec::Opus => "opus",
            Codec::Original => "original",
        }
    }
}

/// Anything a URL can resolve to.
#[derive(Debug, Clone)]
pub enum Entity {
    Track(Box<Track>),
    Playlist(Box<Playlist>),
    User(Box<User>),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track_with_tags(tags: &str) -> Track {
        Track {
            id: 1,
            tag_list: Some(tags.to_string()),
            ..blank_track()
        }
    }

    fn blank_track() -> Track {
        serde_json::from_str(r#"{"id": 1}"#).expect("minimal track should deserialize")
    }

    #[test]
    fn minimal_track_deserializes() {
        let t = blank_track();
        assert_eq!(t.id, 1);
        assert!(!t.downloadable);
        assert!(!t.has_downloads_left);
    }

    #[test]
    fn tag_list_splits_on_spaces() {
        assert_eq!(track_with_tags("house techno").tags(), ["house", "techno"]);
    }

    #[test]
    fn tag_list_honours_quoted_multiword_tags() {
        assert_eq!(
            track_with_tags(r#""deep house" techno "drum and bass""#).tags(),
            ["deep house", "techno", "drum and bass"]
        );
    }

    #[test]
    fn publisher_artist_wins_over_uploader() {
        let mut t = blank_track();
        t.user = Some(User {
            username: Some("some-uploader".into()),
            ..User::default()
        });
        assert_eq!(t.artist(), "some-uploader");
        t.publisher_metadata = Some(PublisherMetadata {
            artist: Some("Real Artist".into()),
            ..PublisherMetadata::default()
        });
        assert_eq!(t.artist(), "Real Artist");
        assert_eq!(t.uploader(), "some-uploader");
    }

    #[test]
    fn blank_publisher_artist_falls_back_to_uploader() {
        let mut t = blank_track();
        t.user = Some(User {
            username: Some("some-uploader".into()),
            ..User::default()
        });
        t.publisher_metadata = Some(PublisherMetadata {
            artist: Some("   ".into()),
            ..PublisherMetadata::default()
        });
        assert_eq!(t.artist(), "some-uploader");
    }

    #[test]
    fn snip_policy_marks_track_as_snipped() {
        let mut t = blank_track();
        assert!(!t.is_snipped());
        t.policy = Some("SNIP".into());
        assert!(t.is_snipped());
    }

    #[test]
    fn unknown_api_fields_are_ignored() {
        let json = r#"{"id": 7, "title": "x", "some_new_field_soundcloud_added": {"a": 1}}"#;
        let t: Track = serde_json::from_str(json).expect("unknown fields must not break parsing");
        assert_eq!(t.title.as_deref(), Some("x"));
    }
}
