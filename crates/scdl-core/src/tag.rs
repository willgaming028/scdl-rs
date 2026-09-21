//! Writing metadata and cover art onto a downloaded file.
//!
//! The tag set mirrors what scdl 3.x writes through its mutagen postprocessor,
//! so files produced by this tool are indistinguishable from the Python
//! version's in any player. lofty handles the per-container mapping (ID3 frames
//! for MP3/AIFF/WAV, iTunes atoms for MP4/M4A, Vorbis comments for FLAC/Opus/Ogg)
//! from one generic [`ItemKey`] set.

use std::path::Path;

use lofty::config::WriteOptions;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::*;
use lofty::tag::{ItemKey, Tag};

use crate::client::Client;
use crate::error::{Error, Result};
use crate::model::Track;

/// Metadata to stamp onto a file, already resolved from a track (plus its
/// playlist context, if any).
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub comment: Option<String>,
    pub composer: Option<String>,
    pub track_number: Option<u32>,
    pub track_total: Option<u32>,
    /// The track's SoundCloud page, written to the URL tag players expose as
    /// "official audio file URL".
    pub url: Option<String>,
    /// `YYYY-MM-DD`.
    pub date: Option<String>,
    pub year: Option<u32>,
    pub license: Option<String>,
    pub label: Option<String>,
    pub isrc: Option<String>,
    pub artwork: Option<Artwork>,
}

#[derive(Clone)]
pub struct Artwork {
    pub data: Vec<u8>,
    pub mime: ArtworkMime,
}

impl std::fmt::Debug for Artwork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Artwork")
            .field("bytes", &self.data.len())
            .field("mime", &self.mime)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtworkMime {
    Jpeg,
    Png,
}

impl ArtworkMime {
    /// Sniff the format from magic bytes rather than trusting Content-Type,
    /// because SoundCloud serves JPEG from `.png` URLs for some size variants.
    pub fn sniff(data: &[u8]) -> Option<Self> {
        if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(ArtworkMime::Jpeg)
        } else if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
            Some(ArtworkMime::Png)
        } else {
            None
        }
    }

    fn to_lofty(self) -> MimeType {
        match self {
            ArtworkMime::Jpeg => MimeType::Jpeg,
            ArtworkMime::Png => MimeType::Png,
        }
    }
}

/// How the artist/album tags should be derived.
#[derive(Debug, Clone, Default)]
pub struct TagOptions {
    /// `--extract-artist`: split "Artist - Title" and use the left side as artist.
    pub extract_artist: bool,
    /// `--no-album-tag`: leave album/album-artist/track-number unset.
    pub no_album_tag: bool,
    /// `--original-metadata`: write nothing at all.
    pub skip: bool,
}

/// The unicode dash variants SoundCloud titles use between artist and title.
/// Mirrors the character class in scdl's `--extract-artist` regex.
const DASHES: &[char] = &['-', '\u{2212}', '\u{2013}', '\u{2014}', '\u{2015}'];

/// Split `"Artist - Title"` into its parts, if the title has that shape.
pub fn split_artist_title(title: &str) -> Option<(String, String)> {
    // Require whitespace before the dash so hyphenated words ("Lo-Fi") survive.
    let mut chars: Vec<(usize, char)> = title.char_indices().collect();
    chars.retain(|(_, c)| !c.is_control());

    // Where the artist portion starts. Advances past a leading track number so
    // that "1 - Artist - Title" yields ("Artist", "Title") rather than
    // ("1 - Artist", "Title").
    let mut start = 0usize;

    for (idx, (byte_idx, c)) in chars.iter().enumerate() {
        if !DASHES.contains(c) || *byte_idx < start {
            continue;
        }
        let prev_is_space = idx > 0 && chars[idx - 1].1.is_whitespace();
        if !prev_is_space {
            continue;
        }
        let artist = title[start..*byte_idx].trim();
        let rest = title[byte_idx + c.len_utf8()..].trim_start();
        if artist.is_empty() || rest.is_empty() {
            continue;
        }
        // A purely numeric left side is a track-number prefix ("1 - Milky Way"),
        // not an artist. The Python version happily tags such a track as
        // `artist=1`; we step over the prefix and keep looking for a real dash.
        // This is a deliberate divergence from upstream.
        if artist
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ')')
        {
            start = byte_idx + c.len_utf8();
            continue;
        }
        return Some((artist.to_string(), rest.to_string()));
    }
    None
}

impl Metadata {
    /// Build the tag set for a track.
    pub fn from_track(track: &Track, opts: &TagOptions) -> Self {
        let mut m = Metadata::default();

        let raw_title = track.title_or_untitled().to_string();
        if opts.extract_artist {
            if let Some((artist, title)) = split_artist_title(&raw_title) {
                m.artist = Some(artist);
                m.title = Some(title);
            }
        }
        m.title.get_or_insert(raw_title);
        m.artist.get_or_insert_with(|| track.artist().to_string());

        m.genre = track.genre.clone().filter(|s| !s.is_empty());
        m.comment = track.description.clone().filter(|s| !s.is_empty());
        m.url = track.permalink_url.clone();
        m.license = track.license.clone();
        m.label = track.label_name.clone();

        if let Some(pm) = &track.publisher_metadata {
            m.isrc = pm.isrc.clone();
            m.composer = pm.writer_composer.clone();
        }

        if let Some(created) = track
            .created_at
            .as_deref()
            .or(track.display_date.as_deref())
        {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(created) {
                m.date = Some(dt.format("%Y-%m-%d").to_string());
                m.year = dt.format("%Y").to_string().parse().ok();
            }
        }

        if !opts.no_album_tag {
            if let Some(ctx) = &track.playlist_context {
                m.album = Some(ctx.title.clone());
                m.album_artist = Some(ctx.uploader.clone());
                m.track_number = Some(ctx.index as u32);
                m.track_total = Some(ctx.total as u32);
            } else if let Some(pm) = &track.publisher_metadata {
                // A single release still has an album title worth keeping.
                m.album = pm.album_title.clone().or_else(|| pm.release_title.clone());
            }
        }

        m
    }

    fn apply_to(&self, tag: &mut Tag) {
        let mut put = |key: ItemKey, value: &Option<String>| {
            if let Some(v) = value.as_deref().filter(|s| !s.trim().is_empty()) {
                // NUL bytes are illegal in every container's text frames.
                tag.insert_text(key, v.replace('\0', ""));
            }
        };

        put(ItemKey::TrackTitle, &self.title);
        put(ItemKey::TrackArtist, &self.artist);
        put(ItemKey::AlbumTitle, &self.album);
        put(ItemKey::AlbumArtist, &self.album_artist);
        put(ItemKey::Genre, &self.genre);
        put(ItemKey::Comment, &self.comment);
        put(ItemKey::Composer, &self.composer);
        put(ItemKey::License, &self.license);
        put(ItemKey::Label, &self.label);
        put(ItemKey::Isrc, &self.isrc);
        // What the Python version writes as WOAF / WWWAUDIOFILE / purl.
        put(ItemKey::AudioFileUrl, &self.url);

        if let Some(n) = self.track_number {
            tag.insert_text(ItemKey::TrackNumber, n.to_string());
        }
        if let Some(n) = self.track_total {
            tag.insert_text(ItemKey::TrackTotal, n.to_string());
        }
        if let Some(d) = &self.date {
            tag.insert_text(ItemKey::RecordingDate, d.clone());
        }
        if let Some(y) = self.year {
            tag.insert_text(ItemKey::Year, y.to_string());
        }

        if let Some(art) = &self.artwork {
            // `unchecked` skips lofty's dimension probing; we have already
            // confirmed the format by magic bytes and do not need width/height.
            let picture = Picture::unchecked(art.data.clone())
                .pic_type(PictureType::CoverFront)
                .mime_type(art.mime.to_lofty())
                .description("Cover (front)")
                .build();
            tag.push_picture(picture);
        }
    }
}

/// Write metadata onto an existing audio file.
pub fn write_tags(path: &Path, meta: &Metadata) -> Result<()> {
    let mut tagged = lofty::read_from_path(path).map_err(|e| Error::tag(path, e))?;

    let tag_type = tagged.primary_tag_type();
    if tagged.primary_tag_mut().is_none() {
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged
        .primary_tag_mut()
        .expect("a tag was just inserted for the primary type");

    meta.apply_to(tag);

    tag.save_to_path(path, WriteOptions::default())
        .map_err(|e| Error::tag(path, e))
}

/// Pick the artwork URL for a track at the requested size.
///
/// SoundCloud encodes the size in the filename (`...-large.jpg`), so a variant
/// is produced by rewriting that trailing token. Two verified quirks drive this:
///
/// * Every non-`original` variant is served as JPEG regardless of the source
///   URL's extension, so those always get `.jpg`.
/// * `original` keeps the *source* extension, and guessing wrong 404s — hence
///   [`artwork_url_flipped`] and the retry in [`fetch_artwork`].
///
/// The token is matched as `-[0-9a-z]+.(jpg|png)` anchored at the end, so a
/// modern mixed-case base id containing a dash is not chewed into.
pub fn artwork_url(track: &Track, original: bool) -> Option<String> {
    let base = artwork_base(track)?;
    let size = if original { "original" } else { "t500x500" };
    let (head, ext) = split_size_token(&base)?;
    // Only "original" preserves the source extension.
    let ext = if original { ext } else { ".jpg" };
    Some(format!("{head}-{size}{ext}"))
}

/// The `original` URL with its extension flipped jpg <-> png.
///
/// SoundCloud stores exactly one of the two and 404s the other, and which one
/// is not predictable from the `artwork_url` field, so a 404 must be retried
/// with the opposite extension before concluding there is no original.
pub fn artwork_url_flipped(track: &Track) -> Option<String> {
    let base = artwork_base(track)?;
    let (head, ext) = split_size_token(&base)?;
    let flipped = if ext.eq_ignore_ascii_case(".png") {
        ".jpg"
    } else {
        ".png"
    };
    Some(format!("{head}-original{flipped}"))
}

fn artwork_base(track: &Track) -> Option<String> {
    track
        .artwork_url
        .clone()
        .or_else(|| track.user.as_ref().and_then(|u| u.avatar_url.clone()))
}

/// Split `...-<token>.<ext>` into everything before the token's dash, and the
/// extension. Only a lowercase-alphanumeric token counts as a size token.
fn split_size_token(url: &str) -> Option<(&str, &str)> {
    let dot = url.rfind('.')?;
    let (path, ext) = url.split_at(dot);
    if !(ext.eq_ignore_ascii_case(".jpg") || ext.eq_ignore_ascii_case(".png")) {
        return None;
    }
    let dash = path.rfind('-')?;
    let token = &path[dash + 1..];
    if token.is_empty()
        || !token
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return None;
    }
    Some((&path[..dash], ext))
}

/// Fetch cover art, returning `Ok(None)` when there is none or it is unusable.
pub async fn fetch_artwork(
    client: &Client,
    track: &Track,
    original: bool,
) -> Result<Option<Artwork>> {
    let Some(url) = artwork_url(track, original) else {
        return Ok(None);
    };

    // For `original`, try the URL as given, then the opposite extension, then
    // give up and take the sized variant rather than embedding nothing.
    let mut candidates = vec![url];
    if original {
        candidates.extend(artwork_url_flipped(track));
        candidates.extend(artwork_url(track, false));
    }

    let mut resp = None;
    for candidate in candidates {
        if let Ok(r) = client.http().get(&candidate).send().await {
            if r.status().is_success() {
                resp = Some(r);
                break;
            }
        }
    }
    let Some(resp) = resp else {
        return Ok(None);
    };

    let data = resp.bytes().await?.to_vec();
    let Some(mime) = ArtworkMime::sniff(&data) else {
        // Neither JPEG nor PNG: the Python version skips these too rather than
        // embedding something players will choke on.
        return Ok(None);
    };

    Ok(Some(Artwork { data, mime }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PlaylistContext, PublisherMetadata, User};

    fn track(title: &str) -> Track {
        let mut t: Track = serde_json::from_str(r#"{"id": 1}"#).unwrap();
        t.title = Some(title.to_string());
        t.user = Some(User {
            username: Some("uploader".into()),
            ..Default::default()
        });
        t.created_at = Some("2015-03-14T09:26:53Z".into());
        t.permalink_url = Some("https://soundcloud.com/u/t".into());
        t
    }

    #[test]
    fn extract_artist_splits_on_spaced_dash() {
        assert_eq!(
            split_artist_title("Boards of Canada - Roygbiv"),
            Some(("Boards of Canada".into(), "Roygbiv".into()))
        );
    }

    #[test]
    fn extract_artist_handles_unicode_dashes() {
        for dash in ["–", "—", "−", "―"] {
            let t = format!("Artist {dash} Title");
            assert_eq!(
                split_artist_title(&t),
                Some(("Artist".into(), "Title".into())),
                "failed for {dash:?}"
            );
        }
    }

    #[test]
    fn extract_artist_leaves_hyphenated_words_alone() {
        // No space before the dash, so this is one title, not artist - title.
        assert_eq!(split_artist_title("Lo-Fi Beats"), None);
        assert_eq!(split_artist_title("Re-Entry"), None);
    }

    #[test]
    fn extract_artist_ignores_leading_or_empty_sides() {
        assert_eq!(split_artist_title(" - Title"), None);
        assert_eq!(split_artist_title("Artist - "), None);
    }

    #[test]
    fn numeric_track_number_prefix_is_not_treated_as_an_artist() {
        // Real case: pandadub's "The Lost Ship" titles its tracks "1 - Milky Way".
        // Upstream tags these as artist="1"; we skip to the next dash, or give up.
        assert_eq!(split_artist_title("1 - Milky Way"), None);
        assert_eq!(split_artist_title("03 - Some Song"), None);
        assert_eq!(
            split_artist_title("1 - Artist - Title"),
            Some(("Artist".into(), "Title".into()))
        );
        // A name that merely starts with a digit is still a valid artist.
        assert_eq!(
            split_artist_title("65daysofstatic - Radio Protector"),
            Some(("65daysofstatic".into(), "Radio Protector".into()))
        );
    }

    #[test]
    fn extract_artist_uses_first_valid_dash_only() {
        assert_eq!(
            split_artist_title("A - B - C"),
            Some(("A".into(), "B - C".into()))
        );
    }

    #[test]
    fn metadata_defaults_to_uploader_when_not_extracting() {
        let m = Metadata::from_track(&track("Artist - Title"), &TagOptions::default());
        assert_eq!(m.title.as_deref(), Some("Artist - Title"));
        assert_eq!(m.artist.as_deref(), Some("uploader"));
    }

    #[test]
    fn metadata_extracts_artist_when_asked() {
        let opts = TagOptions {
            extract_artist: true,
            ..Default::default()
        };
        let m = Metadata::from_track(&track("Artist - Title"), &opts);
        assert_eq!(m.title.as_deref(), Some("Title"));
        assert_eq!(m.artist.as_deref(), Some("Artist"));
    }

    #[test]
    fn playlist_context_populates_album_tags() {
        let mut t = track("Song");
        t.playlist_context = Some(PlaylistContext {
            id: 5,
            title: "The Lost Ship".into(),
            uploader: "pandadub".into(),
            index: 3,
            total: 10,
        });
        let m = Metadata::from_track(&t, &TagOptions::default());
        assert_eq!(m.album.as_deref(), Some("The Lost Ship"));
        assert_eq!(m.album_artist.as_deref(), Some("pandadub"));
        assert_eq!(m.track_number, Some(3));
        assert_eq!(m.track_total, Some(10));
    }

    #[test]
    fn no_album_tag_suppresses_album_fields() {
        let mut t = track("Song");
        t.playlist_context = Some(PlaylistContext {
            id: 5,
            title: "P".into(),
            uploader: "u".into(),
            index: 1,
            total: 2,
        });
        let opts = TagOptions {
            no_album_tag: true,
            ..Default::default()
        };
        let m = Metadata::from_track(&t, &opts);
        assert!(m.album.is_none());
        assert!(m.album_artist.is_none());
        assert!(m.track_number.is_none());
    }

    #[test]
    fn date_is_iso_8601() {
        let m = Metadata::from_track(&track("x"), &TagOptions::default());
        assert_eq!(m.date.as_deref(), Some("2015-03-14"));
        assert_eq!(m.year, Some(2015));
    }

    #[test]
    fn publisher_metadata_supplies_isrc_and_composer() {
        let mut t = track("x");
        t.publisher_metadata = Some(PublisherMetadata {
            isrc: Some("GBAYE0601498".into()),
            writer_composer: Some("A Composer".into()),
            ..Default::default()
        });
        let m = Metadata::from_track(&t, &TagOptions::default());
        assert_eq!(m.isrc.as_deref(), Some("GBAYE0601498"));
        assert_eq!(m.composer.as_deref(), Some("A Composer"));
    }

    #[test]
    fn artwork_url_rewrites_the_size_token() {
        let mut t = track("x");
        t.artwork_url = Some("https://i1.sndcdn.com/artworks-abc123-0-large.jpg".into());
        assert_eq!(
            artwork_url(&t, false).as_deref(),
            Some("https://i1.sndcdn.com/artworks-abc123-0-t500x500.jpg")
        );
        assert_eq!(
            artwork_url(&t, true).as_deref(),
            Some("https://i1.sndcdn.com/artworks-abc123-0-original.jpg")
        );
    }

    #[test]
    fn artwork_url_falls_back_to_user_avatar() {
        let mut t = track("x");
        t.user = Some(User {
            avatar_url: Some("https://i1.sndcdn.com/avatars-xyz-large.jpg".into()),
            ..Default::default()
        });
        assert_eq!(
            artwork_url(&t, false).as_deref(),
            Some("https://i1.sndcdn.com/avatars-xyz-t500x500.jpg")
        );
    }

    #[test]
    fn artwork_mime_sniffing_ignores_extension() {
        assert_eq!(
            ArtworkMime::sniff(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(ArtworkMime::Jpeg)
        );
        assert_eq!(
            ArtworkMime::sniff(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
            Some(ArtworkMime::Png)
        );
        assert_eq!(ArtworkMime::sniff(b"GIF89a"), None);
        assert_eq!(ArtworkMime::sniff(b""), None);
    }
}

#[cfg(test)]
mod artwork_tests {
    use super::*;
    use crate::model::User;

    fn track_with_art(url: &str) -> Track {
        let mut t: Track = serde_json::from_str(r#"{"id":1}"#).unwrap();
        t.artwork_url = Some(url.to_string());
        t
    }

    #[test]
    fn original_extension_is_flipped_for_retry() {
        let t = track_with_art("https://i1.sndcdn.com/artworks-abc-large.png");
        assert_eq!(
            artwork_url(&t, true).as_deref(),
            Some("https://i1.sndcdn.com/artworks-abc-original.png")
        );
        assert_eq!(
            artwork_url_flipped(&t).as_deref(),
            Some("https://i1.sndcdn.com/artworks-abc-original.jpg")
        );
    }

    #[test]
    fn sized_variants_are_always_jpeg_even_from_a_png_source() {
        let t = track_with_art("https://i1.sndcdn.com/artworks-abc-large.png");
        assert_eq!(
            artwork_url(&t, false).as_deref(),
            Some("https://i1.sndcdn.com/artworks-abc-t500x500.jpg")
        );
    }

    #[test]
    fn mixed_case_base_ids_with_dashes_are_not_chewed_into() {
        // The token charset is lowercase+digits precisely so a modern id like
        // "a1wKGMYNreDLTMrT-fGjRiw" keeps its internal dash.
        let t = track_with_art("https://i1.sndcdn.com/artworks-a1wKGMYNreDLTMrT-fGjRiw-large.png");
        assert_eq!(
            artwork_url(&t, false).as_deref(),
            Some("https://i1.sndcdn.com/artworks-a1wKGMYNreDLTMrT-fGjRiw-t500x500.jpg")
        );
    }

    #[test]
    fn non_image_urls_are_rejected() {
        let t = track_with_art("https://i1.sndcdn.com/artworks-abc-large.gif");
        assert!(artwork_url(&t, false).is_none());
    }

    #[test]
    fn falls_back_to_the_user_avatar() {
        let mut t: Track = serde_json::from_str(r#"{"id":1}"#).unwrap();
        t.user = Some(User {
            avatar_url: Some("https://i1.sndcdn.com/avatars-xyz-large.jpg".into()),
            ..Default::default()
        });
        assert_eq!(
            artwork_url(&t, false).as_deref(),
            Some("https://i1.sndcdn.com/avatars-xyz-t500x500.jpg")
        );
    }
}
