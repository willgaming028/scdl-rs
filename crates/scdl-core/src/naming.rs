//! Output filename construction: templating, sanitization, truncation.
//!
//! Two template syntaxes are supported so existing configs keep working:
//!
//! * `%(field)s` — what scdl 3.x uses, inherited from yt-dlp's output template.
//! * `{field}` — the scdl 2.x syntax, including the nested `{user[username]}`
//!   and `{playlist[title]}` forms.
//!
//! Both are normalised to the same field set before substitution.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::Track;

/// scdl's historical default, from the shipped `scdl.cfg`.
pub const DEFAULT_NAME_FORMAT: &str = "[%(id)s] %(uploader)s - %(title)s.%(ext)s";
/// scdl's historical default for tracks downloaded as part of a playlist.
pub const DEFAULT_PLAYLIST_NAME_FORMAT: &str =
    "%(playlist_index)s. %(uploader)s - %(title)s.%(ext)s";

/// Maximum filename length in **bytes**, matching scdl 3.x's `--trim-filenames 240b`.
/// Bytes rather than characters because that is what filesystems actually limit
/// (ext4 caps a name at 255 bytes, and a name of 240 multi-byte characters would
/// blow past it).
pub const MAX_FILENAME_BYTES: usize = 240;

/// The legacy `{field}` spellings and what they map to.
const LEGACY_FIELD_MAP: &[(&str, &str)] = &[
    ("{id}", "%(id)s"),
    ("{user[username]}", "%(uploader)s"),
    ("{user[id]}", "%(uploader_id)s"),
    ("{user[permalink_url]}", "%(uploader_url)s"),
    ("{timestamp}", "%(timestamp)s"),
    ("{title}", "%(title)s"),
    ("{description}", "%(description)s"),
    ("{duration}", "%(duration)s"),
    ("{permalink_url}", "%(webpage_url)s"),
    ("{license}", "%(license)s"),
    ("{playback_count}", "%(view_count)s"),
    ("{likes_count}", "%(like_count)s"),
    ("{comment_count}", "%(comment_count)s"),
    ("{reposts_count}", "%(repost_count)s"),
    ("{playlist[author]}", "%(playlist_uploader)s"),
    ("{playlist[title]}", "%(playlist)s"),
    ("{playlist[id]}", "%(playlist_id)s"),
    ("{playlist[tracknumber]}", "%(playlist_index)s"),
    ("{playlist[tracknumber_total]}", "%(playlist_count)s"),
];

/// Rewrite a legacy `{field}` template into `%(field)s` form.
///
/// Templates already in `%(field)s` form pass through untouched.
pub fn convert_legacy_format(s: &str) -> String {
    let mut out = s.to_string();
    for (old, new) in LEGACY_FIELD_MAP {
        if out.contains(old) {
            out = out.replace(old, new);
        }
    }
    out
}

/// Every field a template may reference.
#[derive(Debug, Clone, Default)]
pub struct Fields(HashMap<String, String>);

impl Fields {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        self.0.insert(key.to_string(), value.into());
    }

    /// Build the field set for a track, with `ext` supplied by the chosen format.
    pub fn from_track(track: &Track, ext: &str) -> Self {
        let mut f = Fields::default();

        f.set("id", track.id.to_string());
        f.set("title", track.title_or_untitled());
        f.set("track", track.title_or_untitled());
        f.set("ext", ext);
        f.set("uploader", track.uploader());
        f.set("artist", track.artist());

        if let Some(u) = &track.user {
            if let Some(id) = u.id {
                f.set("uploader_id", id.to_string());
            }
            if let Some(url) = &u.permalink_url {
                f.set("uploader_url", url.clone());
            }
        }
        if let Some(url) = &track.permalink_url {
            f.set("webpage_url", url.clone());
            f.set("permalink_url", url.clone());
        }
        if let Some(d) = &track.description {
            f.set("description", d.clone());
        }
        if let Some(g) = &track.genre {
            f.set("genre", g.clone());
        }
        if let Some(l) = &track.license {
            f.set("license", l.clone());
        }
        if let Some(d) = track.duration {
            f.set("duration", (d / 1000).to_string());
        }
        if let Some(c) = track.playback_count {
            f.set("view_count", c.to_string());
        }
        if let Some(c) = track.likes_count.or(track.favoritings_count) {
            f.set("like_count", c.to_string());
        }
        if let Some(c) = track.comment_count {
            f.set("comment_count", c.to_string());
        }
        if let Some(c) = track.reposts_count {
            f.set("repost_count", c.to_string());
            // scdl 3.x's converter has a typo (`respost_count`); accept both so
            // a config written against the buggy spelling still resolves.
            f.set("respost_count", c.to_string());
        }

        // Timestamps: `timestamp` is a unix epoch, `upload_date` is YYYYMMDD.
        if let Some(created) = track
            .created_at
            .as_deref()
            .or(track.display_date.as_deref())
        {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(created) {
                f.set("timestamp", dt.timestamp().to_string());
                f.set("upload_date", dt.format("%Y%m%d").to_string());
                f.set("release_date", dt.format("%Y-%m-%d").to_string());
                f.set("year", dt.format("%Y").to_string());
            }
        }

        if let Some(ctx) = &track.playlist_context {
            f.set("playlist", ctx.title.clone());
            f.set("playlist_title", ctx.title.clone());
            f.set("playlist_id", ctx.id.to_string());
            f.set("playlist_uploader", ctx.uploader.clone());
            f.set("playlist_index", ctx.index.to_string());
            f.set("playlist_count", ctx.total.to_string());
            f.set(
                "playlist_autonumber",
                format!("{:0width$}", ctx.index, width = ctx.total.to_string().len()),
            );
        }

        f
    }
}

/// Render a `%(field)s` template.
///
/// Unknown fields render empty rather than erroring, matching scdl's
/// `--output-na-placeholder ""`. Supports yt-dlp's zero-padding spec
/// (`%(playlist_index)02d`) for the numeric fields where it is commonly used.
pub fn render(template: &str, fields: &Fields) -> Result<String> {
    let normalized = convert_legacy_format(template);
    let mut out = String::with_capacity(normalized.len() + 32);
    let bytes: Vec<char> = normalized.chars().collect();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != '%' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        // "%%" is a literal percent.
        if i + 1 < bytes.len() && bytes[i + 1] == '%' {
            out.push('%');
            i += 2;
            continue;
        }
        if i + 1 >= bytes.len() || bytes[i + 1] != '(' {
            out.push('%');
            i += 1;
            continue;
        }

        let Some(close) = bytes[i + 2..].iter().position(|c| *c == ')') else {
            return Err(Error::Template(format!(
                "unterminated field in template: {template:?}"
            )));
        };
        let name: String = bytes[i + 2..i + 2 + close].iter().collect();
        let mut j = i + 2 + close + 1;

        // Consume an optional conversion spec, e.g. `02d` or `s`.
        let mut spec = String::new();
        while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == '0') {
            spec.push(bytes[j]);
            j += 1;
        }
        if j < bytes.len() && matches!(bytes[j], 's' | 'd' | 'j' | 'B' | 'f') {
            j += 1;
        }

        let value = fields.get(&name).unwrap_or("");
        if let Ok(width) = spec.parse::<usize>() {
            if let Ok(n) = value.parse::<i64>() {
                out.push_str(&format!("{n:0width$}"));
            } else {
                out.push_str(value);
            }
        } else {
            out.push_str(value);
        }
        i = j;
    }

    Ok(out)
}

/// Make a single path component safe to write.
///
/// Replaces separators and control characters, neutralises `.`/`..`, avoids
/// Windows reserved device names, and trims trailing dots and spaces (which
/// Windows silently strips, causing collisions).
pub fn sanitize_component(s: &str) -> String {
    const WINDOWS_RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let replacement = match c {
            // Path separators are the traversal vector; never let them through.
            '/' | '\\' => '\u{29F8}', // BIG SOLIDUS — visually similar, not a separator
            ':' => '\u{A789}',        // MODIFIER LETTER COLON
            '*' => '\u{2731}',
            '?' => '\u{FF1F}',
            '"' => '\u{2033}',
            '<' => '\u{FF1C}',
            '>' => '\u{FF1E}',
            '|' => '\u{FF5C}',
            c if (c as u32) < 0x20 || c == '\u{7F}' => ' ',
            c => c,
        };
        out.push(replacement);
    }

    // Trailing dots/spaces are dropped by Windows; strip them ourselves so the
    // name we compute is the name we get. A name that is *entirely* dots and
    // spaces (".", "..", "...") collapses to empty here, which is exactly what
    // we want — it becomes the placeholder below rather than a directory ref.
    let mut out = out.trim_matches(|c: char| c == ' ' || c == '.').to_string();

    if out.is_empty() {
        out = "_".to_string();
    }

    let stem_upper = out.split('.').next().unwrap_or(&out).to_ascii_uppercase();
    if WINDOWS_RESERVED.contains(&stem_upper.as_str()) {
        out.insert(0, '_');
    }

    out
}

/// Truncate to `MAX_FILENAME_BYTES`, preserving the extension and never
/// splitting a UTF-8 character.
pub fn truncate_filename(name: &str, max_bytes: usize) -> String {
    if name.len() <= max_bytes {
        return name.to_string();
    }

    let (stem, ext) = match name.rfind('.') {
        // Only treat a short trailing run as an extension; a dot deep in a long
        // title is part of the title.
        Some(i) if name.len() - i <= 12 && i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };

    let budget = max_bytes.saturating_sub(ext.len());
    let mut end = budget.min(stem.len());
    while end > 0 && !stem.is_char_boundary(end) {
        end -= 1;
    }

    format!("{}{}", &stem[..end], ext)
}

/// Build the final output path for a track and verify it stays inside `base`.
///
/// This is the security boundary: remote-controlled values (track title,
/// playlist name, uploader) feed the template, so the result is checked against
/// `base` before anything is written.
pub fn build_output_path(
    base: &Path,
    template: &str,
    fields: &Fields,
    playlist_subfolder: Option<&str>,
) -> Result<PathBuf> {
    // Split the TEMPLATE on separators before substituting, never the rendered
    // result. This is the whole security property: a separator the template
    // author typed creates a subdirectory, while a separator that arrives inside
    // a remote-controlled field value (track title, playlist name, uploader) is
    // sanitized into a harmless character by `sanitize_component` below. Doing
    // this the other way round lets a track titled "../../etc/passwd" dictate
    // the directory layout.
    let normalized = convert_legacy_format(template);
    let template_parts: Vec<&str> = normalized
        .split(['/', '\\'])
        .filter(|p| !p.trim().is_empty())
        .collect();

    if template_parts.is_empty() {
        return Err(Error::Template(format!(
            "template {template:?} produced an empty filename"
        )));
    }

    let mut path = base.to_path_buf();

    if let Some(folder) = playlist_subfolder {
        let folder = truncate_filename(&sanitize_component(folder), MAX_FILENAME_BYTES);
        path.push(folder);
    }

    for part in &template_parts {
        let rendered = render(part, fields)?;
        let clean = truncate_filename(&sanitize_component(&rendered), MAX_FILENAME_BYTES);
        path.push(clean);
    }

    // Belt and braces: sanitization should already make this impossible, but the
    // cost of being wrong is writing outside the user's chosen directory.
    if !is_within(base, &path) {
        return Err(Error::UnsafePath(path));
    }

    Ok(path)
}

/// True when `candidate` is `base` or lies beneath it, judged lexically on
/// normalised components (no filesystem access, so it works for paths that do
/// not exist yet).
pub fn is_within(base: &Path, candidate: &Path) -> bool {
    let norm = |p: &Path| -> Vec<std::ffi::OsString> {
        let mut out: Vec<std::ffi::OsString> = Vec::new();
        for c in p.components() {
            match c {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                Component::Normal(s) => out.push(s.to_os_string()),
                Component::RootDir => out.push(std::ffi::OsString::from("/")),
                Component::Prefix(p) => out.push(p.as_os_str().to_os_string()),
            }
        }
        out
    };

    let b = norm(base);
    let c = norm(candidate);
    c.len() >= b.len() && c[..b.len()] == b[..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PlaylistContext;

    fn track() -> Track {
        let mut t: Track =
            serde_json::from_str(r#"{"id": 12345, "title": "Low Extender"}"#).unwrap();
        t.user = Some(crate::model::User {
            id: Some(999),
            username: Some("jumpstreetpsy".into()),
            permalink_url: Some("https://soundcloud.com/jumpstreetpsy".into()),
            ..Default::default()
        });
        t.permalink_url = Some("https://soundcloud.com/jumpstreetpsy/low-extender".into());
        t.created_at = Some("2015-03-14T09:26:53Z".into());
        t
    }

    #[test]
    fn renders_the_shipped_default_format() {
        let f = Fields::from_track(&track(), "mp3");
        assert_eq!(
            render(DEFAULT_NAME_FORMAT, &f).unwrap(),
            "[12345] jumpstreetpsy - Low Extender.mp3"
        );
    }

    #[test]
    fn renders_the_shipped_default_playlist_format() {
        let mut t = track();
        t.playlist_context = Some(PlaylistContext {
            id: 7,
            title: "The Lost Ship".into(),
            uploader: "pandadub".into(),
            index: 3,
            total: 10,
        });
        let f = Fields::from_track(&t, "m4a");
        assert_eq!(
            render(DEFAULT_PLAYLIST_NAME_FORMAT, &f).unwrap(),
            "3. jumpstreetpsy - Low Extender.m4a"
        );
    }

    #[test]
    fn legacy_brace_syntax_still_works() {
        let f = Fields::from_track(&track(), "mp3");
        assert_eq!(
            render("{user[username]} - {title}.{ext}", &f).unwrap(),
            // {ext} has no legacy mapping in upstream scdl either, so it stays literal.
            "jumpstreetpsy - Low Extender.{ext}"
        );
        assert_eq!(
            render("{id} - {title}", &f).unwrap(),
            "12345 - Low Extender"
        );
    }

    #[test]
    fn upstreams_respost_count_typo_is_accepted() {
        let mut t = track();
        t.reposts_count = Some(42);
        let f = Fields::from_track(&t, "mp3");
        assert_eq!(render("%(respost_count)s", &f).unwrap(), "42");
        assert_eq!(render("%(repost_count)s", &f).unwrap(), "42");
    }

    #[test]
    fn unknown_fields_render_empty_not_error() {
        let f = Fields::from_track(&track(), "mp3");
        assert_eq!(render("a%(nope)sb", &f).unwrap(), "ab");
    }

    #[test]
    fn zero_padding_spec_is_honoured() {
        let mut t = track();
        t.playlist_context = Some(PlaylistContext {
            id: 1,
            title: "p".into(),
            uploader: "u".into(),
            index: 3,
            total: 100,
        });
        let f = Fields::from_track(&t, "mp3");
        assert_eq!(render("%(playlist_index)03d", &f).unwrap(), "003");
    }

    #[test]
    fn literal_percent_is_preserved() {
        let f = Fields::from_track(&track(), "mp3");
        assert_eq!(render("100%% pure", &f).unwrap(), "100% pure");
    }

    // --- Safety: these are the tests that matter ---

    #[test]
    fn separators_in_field_values_cannot_add_path_depth() {
        let mut t = track();
        t.title = Some("../../etc/passwd".into());
        let f = Fields::from_track(&t, "mp3");
        let base = Path::new("/home/u/Music");
        let p = build_output_path(base, "%(title)s.%(ext)s", &f, None).unwrap();
        assert!(is_within(base, &p), "escaped to {}", p.display());
        // Not merely contained: the remote title must not create directories at all.
        assert_eq!(p.parent().unwrap(), base, "remote value added path depth");
        assert!(!p.to_string_lossy().contains("etc/passwd"));
    }

    #[test]
    fn a_playlist_named_dotdot_cannot_escape() {
        // This is the exact traversal the Python version is vulnerable to.
        let f = Fields::from_track(&track(), "mp3");
        let base = Path::new("/home/u/Music");
        let p = build_output_path(base, "%(title)s.%(ext)s", &f, Some("..")).unwrap();
        assert!(is_within(base, &p), "escaped to {}", p.display());
        assert_eq!(p.parent().unwrap(), base.join("_"));
    }

    #[test]
    fn absolute_paths_in_field_values_are_neutralised() {
        let mut t = track();
        t.title = Some("/etc/cron.d/evil".into());
        let f = Fields::from_track(&t, "mp3");
        let base = Path::new("/home/u/Music");
        let p = build_output_path(base, "%(title)s.%(ext)s", &f, None).unwrap();
        assert!(is_within(base, &p), "escaped to {}", p.display());
    }

    #[test]
    fn template_authored_subdirectories_are_allowed() {
        let f = Fields::from_track(&track(), "mp3");
        let base = Path::new("/home/u/Music");
        let p = build_output_path(base, "%(uploader)s/%(title)s.%(ext)s", &f, None).unwrap();
        assert_eq!(p, base.join("jumpstreetpsy").join("Low Extender.mp3"));
    }

    #[test]
    fn sanitize_neutralises_dot_and_dotdot() {
        assert_eq!(sanitize_component(".."), "_");
        assert_eq!(sanitize_component("."), "_");
        assert_eq!(sanitize_component("..."), "_");
    }

    #[test]
    fn sanitize_handles_control_chars_and_windows_reserved() {
        assert_eq!(sanitize_component("a\u{0}b\nc"), "a b c");
        assert_eq!(sanitize_component("CON"), "_CON");
        assert_eq!(sanitize_component("con.mp3"), "_con.mp3");
        assert_eq!(sanitize_component("trailing. "), "trailing");
    }

    #[test]
    fn truncation_respects_byte_budget_and_utf8_boundaries() {
        let name = format!("{}.mp3", "é".repeat(200)); // 400 bytes of stem
        let t = truncate_filename(&name, MAX_FILENAME_BYTES);
        assert!(t.len() <= MAX_FILENAME_BYTES, "len was {}", t.len());
        assert!(t.ends_with(".mp3"));
        assert!(std::str::from_utf8(t.as_bytes()).is_ok());
    }

    #[test]
    fn truncation_leaves_short_names_alone() {
        assert_eq!(truncate_filename("short.mp3", 240), "short.mp3");
    }

    #[test]
    fn is_within_rejects_sibling_prefix_directories() {
        // "/home/u/Music-evil" must not count as inside "/home/u/Music".
        assert!(!is_within(
            Path::new("/home/u/Music"),
            Path::new("/home/u/Music-evil/x.mp3")
        ));
        assert!(is_within(
            Path::new("/home/u/Music"),
            Path::new("/home/u/Music/x.mp3")
        ));
    }
}
