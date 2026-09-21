//! Command-line interface.
//!
//! Flag names and semantics follow the Python scdl so existing scripts and
//! muscle memory keep working, including the single-dash short flags
//! (`-a -t -f -C -p -r`) that came from its docopt usage string.

use std::path::PathBuf;

use clap::{ArgAction, Parser};

#[derive(Parser, Debug, Clone)]
#[command(
    name = "scdl",
    version,
    about = "Download music from SoundCloud",
    long_about = "Download tracks, playlists, likes and reposts from SoundCloud.\n\
                  Run with no arguments to open the interactive terminal UI.",
    after_help = "EXAMPLES:\n  \
        scdl                                        open the interactive UI\n  \
        scdl -l https://soundcloud.com/user/track   download one track\n  \
        scdl -l https://soundcloud.com/user -a      download everything a user posted\n  \
        scdl -l https://soundcloud.com/user -f      download a user's likes\n  \
        scdl -s \"aphex twin\"                        search and download the top hit\n  \
        scdl me -f                                  download your own likes (needs --auth-token)"
)]
pub struct Cli {
    /// URL of a track, playlist, or user
    #[arg(short = 'l', long = "url", value_name = "URL")]
    pub url: Option<String>,

    /// Search for a track/playlist/user and use the first result
    #[arg(short = 's', long = "search", value_name = "QUERY")]
    pub search: Option<String>,

    /// Use the profile associated with your auth token
    #[arg(value_name = "me", value_parser = ["me"])]
    pub me: Option<String>,

    // --- What to download from a user ---
    /// Download all tracks of a user, including reposts
    #[arg(short = 'a', action = ArgAction::SetTrue, group = "selector")]
    pub all: bool,

    /// Download only a user's own uploads (no reposts)
    #[arg(short = 't', action = ArgAction::SetTrue, group = "selector")]
    pub tracks: bool,

    /// Download a user's likes
    #[arg(short = 'f', action = ArgAction::SetTrue, group = "selector")]
    pub favorites: bool,

    /// Download tracks a user commented on
    #[arg(short = 'C', action = ArgAction::SetTrue, group = "selector")]
    pub commented: bool,

    /// Download a user's playlists
    #[arg(short = 'p', action = ArgAction::SetTrue, group = "selector")]
    pub playlists: bool,

    /// Download a user's reposts
    #[arg(short = 'r', action = ArgAction::SetTrue, group = "selector")]
    pub reposts: bool,

    // --- Output ---
    /// Directory to download into
    #[arg(long, value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Filename format. Accepts %(field)s and legacy {field} syntax
    #[arg(long = "name-format", value_name = "FORMAT")]
    pub name_format: Option<String>,

    /// Filename format for tracks downloaded as part of a playlist
    #[arg(long = "playlist-name-format", value_name = "FORMAT")]
    pub playlist_name_format: Option<String>,

    /// Put playlist tracks in the main directory instead of a subfolder
    #[arg(long = "no-playlist-folder", action = ArgAction::SetTrue)]
    pub no_playlist_folder: bool,

    /// Start downloading a playlist from the Nth track (1-indexed)
    #[arg(short = 'o', long = "offset", value_name = "N")]
    pub offset: Option<usize>,

    // --- Existing files ---
    /// Skip tracks that have already been downloaded
    #[arg(short = 'c', long = "continue", action = ArgAction::SetTrue)]
    pub continue_existing: bool,

    /// Overwrite files that already exist
    #[arg(long, action = ArgAction::SetTrue)]
    pub overwrite: bool,

    /// Re-apply metadata to files that are already downloaded
    #[arg(long = "force-metadata", action = ArgAction::SetTrue)]
    pub force_metadata: bool,

    // --- Archive / sync ---
    /// Record downloaded track IDs here and skip anything already listed
    #[arg(long = "download-archive", value_name = "FILE")]
    pub download_archive: Option<PathBuf>,

    /// Mirror a playlist: download new tracks AND DELETE removed ones
    #[arg(long, value_name = "FILE")]
    pub sync: Option<PathBuf>,

    /// Allow --sync to proceed when the playlist comes back empty (dangerous)
    #[arg(long = "sync-allow-empty", action = ArgAction::SetTrue)]
    pub sync_allow_empty: bool,

    /// Let --sync delete any number of files, bypassing the safety limit
    #[arg(long = "sync-force", action = ArgAction::SetTrue)]
    pub sync_force: bool,

    /// Show what --sync would do without touching anything
    #[arg(long = "dry-run", action = ArgAction::SetTrue)]
    pub dry_run: bool,

    // --- Format selection ---
    /// Download only MP3 streams
    #[arg(long = "onlymp3", action = ArgAction::SetTrue)]
    pub only_mp3: bool,

    /// Prefer Opus streams (excluded by default)
    #[arg(long, action = ArgAction::SetTrue)]
    pub opus: bool,

    /// Never download the uploader's original file
    #[arg(long = "no-original", action = ArgAction::SetTrue)]
    pub no_original: bool,

    /// Only download tracks that offer an original file
    #[arg(long = "only-original", action = ArgAction::SetTrue)]
    pub only_original: bool,

    /// Skip tracks smaller than this (e.g. 500k, 5m)
    #[arg(long = "min-size", value_name = "SIZE")]
    pub min_size: Option<String>,

    /// Skip tracks larger than this (e.g. 500k, 5m)
    #[arg(long = "max-size", value_name = "SIZE")]
    pub max_size: Option<String>,

    // --- Metadata ---
    /// Take the artist from the title ("Artist - Title") instead of the uploader
    #[arg(long = "extract-artist", action = ArgAction::SetTrue)]
    pub extract_artist: bool,

    /// Do not write album tags
    #[arg(long = "no-album-tag", action = ArgAction::SetTrue)]
    pub no_album_tag: bool,

    /// Do not write any metadata
    #[arg(long = "original-metadata", action = ArgAction::SetTrue)]
    pub original_metadata: bool,

    /// Embed the full-size cover instead of the 500x500 version
    #[arg(long = "original-art", action = ArgAction::SetTrue)]
    pub original_art: bool,

    /// Write each track's description to a .txt file beside it
    #[arg(long = "add-description", action = ArgAction::SetTrue)]
    pub add_description: bool,

    /// Convert lossless original files to FLAC (needs ffmpeg)
    #[arg(long, action = ArgAction::SetTrue)]
    pub flac: bool,

    /// Keep the uploader's own filename for original-file downloads
    #[arg(long = "original-name", action = ArgAction::SetTrue)]
    pub original_name: bool,

    /// Prefix the filename with the artist if it is missing (legacy)
    #[arg(long = "addtofile", action = ArgAction::SetTrue)]
    pub addtofile: bool,

    /// Prefix the filename with the upload timestamp (legacy; prefer --name-format)
    #[arg(long = "addtimestamp", action = ArgAction::SetTrue)]
    pub addtimestamp: bool,

    /// Suppress warnings
    #[arg(long = "hidewarnings", action = ArgAction::SetTrue)]
    pub hidewarnings: bool,

    // --- Auth ---
    /// SoundCloud client_id to use (one is scraped automatically if omitted)
    #[arg(long = "client-id", value_name = "ID")]
    pub client_id: Option<String>,

    /// OAuth token. Prefer the SCDL_AUTH_TOKEN environment variable
    #[arg(
        long = "auth-token",
        value_name = "TOKEN",
        env = "SCDL_AUTH_TOKEN",
        hide_env_values = true
    )]
    pub auth_token: Option<String>,

    // --- Behaviour ---
    /// Abort the whole run if any track fails
    #[arg(long = "strict-playlist", action = ArgAction::SetTrue)]
    pub strict: bool,

    /// Refuse to download playlists
    #[arg(long = "no-playlist", action = ArgAction::SetTrue)]
    pub no_playlist: bool,

    /// Number of tracks to download at once
    #[arg(short = 'j', long, value_name = "N", default_value_t = 3)]
    pub jobs: usize,

    /// Force the interactive terminal UI
    #[arg(long, action = ArgAction::SetTrue)]
    pub tui: bool,

    /// Never use the interactive UI, even with no arguments
    #[arg(long = "no-tui", action = ArgAction::SetTrue)]
    pub no_tui: bool,

    /// Hide the progress display
    #[arg(long = "hide-progress", action = ArgAction::SetTrue)]
    pub hide_progress: bool,

    /// Verbose logging. Credentials are redacted
    #[arg(long, action = ArgAction::SetTrue)]
    pub debug: bool,

    /// Only log errors
    #[arg(long, action = ArgAction::SetTrue)]
    pub error: bool,
}

impl Cli {
    /// Which user collection the selector flags request.
    pub fn selector(&self) -> scdl_core::resolve::Selector {
        use scdl_core::resolve::Selector;
        if self.all {
            Selector::All
        } else if self.favorites {
            Selector::Likes
        } else if self.commented {
            Selector::Comments
        } else if self.playlists {
            Selector::Playlists
        } else if self.reposts {
            Selector::Reposts
        } else {
            Selector::Tracks
        }
    }

    /// True when nothing was asked for, so the UI should open.
    pub fn should_use_tui(&self) -> bool {
        if self.no_tui {
            return false;
        }
        if self.tui {
            return true;
        }
        self.url.is_none() && self.search.is_none() && self.me.is_none()
    }

    pub fn target(&self) -> Option<scdl_core::resolve::Target> {
        use scdl_core::resolve::Target;
        if self.me.is_some() {
            Some(Target::Me)
        } else if let Some(q) = &self.search {
            Some(Target::Search(q.clone()))
        } else {
            self.url.clone().map(Target::Url)
        }
    }
}

/// Parse a size like `500k`, `5m`, `1g`, or a plain byte count.
pub fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty size".to_string());
    }

    let (num, mult) = match s.chars().last().unwrap().to_ascii_lowercase() {
        'k' => (&s[..s.len() - 1], 1024u64),
        'm' => (&s[..s.len() - 1], 1024 * 1024),
        'g' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        'b' => (&s[..s.len() - 1], 1),
        _ => (s, 1),
    };

    num.trim()
        .parse::<f64>()
        .map_err(|_| format!("{s:?} is not a valid size"))
        .and_then(|v| {
            if v < 0.0 {
                Err(format!("{s:?} is negative"))
            } else {
                Ok((v * mult as f64) as u64)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use scdl_core::resolve::Selector;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_the_readme_examples() {
        // scdl -l https://soundcloud.com/quanta-uk -a
        let c =
            Cli::try_parse_from(["scdl", "-l", "https://soundcloud.com/quanta-uk", "-a"]).unwrap();
        assert_eq!(c.url.as_deref(), Some("https://soundcloud.com/quanta-uk"));
        assert_eq!(c.selector(), Selector::All);

        // scdl -l https://soundcloud.com/kobiblastoyz -f
        let c = Cli::try_parse_from(["scdl", "-l", "https://soundcloud.com/kobiblastoyz", "-f"])
            .unwrap();
        assert_eq!(c.selector(), Selector::Likes);

        // scdl me -f
        let c = Cli::try_parse_from(["scdl", "me", "-f"]).unwrap();
        assert_eq!(c.me.as_deref(), Some("me"));
        assert_eq!(c.selector(), Selector::Likes);
    }

    #[test]
    fn every_legacy_selector_short_flag_works() {
        for (flag, expected) in [
            ("-a", Selector::All),
            ("-t", Selector::Tracks),
            ("-f", Selector::Likes),
            ("-C", Selector::Comments),
            ("-p", Selector::Playlists),
            ("-r", Selector::Reposts),
        ] {
            let c = Cli::try_parse_from(["scdl", "-l", "https://soundcloud.com/u", flag]).unwrap();
            assert_eq!(c.selector(), expected, "flag {flag} mapped wrong");
        }
    }

    #[test]
    fn selector_flags_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["scdl", "-l", "u", "-a", "-f"]).is_err());
    }

    #[test]
    fn legacy_long_flags_parse() {
        let c = Cli::try_parse_from([
            "scdl",
            "-l",
            "u",
            "-c",
            "--onlymp3",
            "--extract-artist",
            "--no-album-tag",
            "--original-art",
            "--add-description",
            "--strict-playlist",
            "--no-playlist",
            "--no-playlist-folder",
            "--force-metadata",
            "--overwrite",
            "--opus",
            "--hide-progress",
        ])
        .unwrap();
        assert!(c.continue_existing && c.only_mp3 && c.extract_artist && c.no_album_tag);
        assert!(c.original_art && c.add_description && c.strict && c.no_playlist);
        assert!(c.no_playlist_folder && c.force_metadata && c.overwrite && c.opus);
        assert!(c.hide_progress);
    }

    #[test]
    fn tui_opens_only_when_nothing_was_requested() {
        assert!(Cli::try_parse_from(["scdl"]).unwrap().should_use_tui());
        assert!(!Cli::try_parse_from(["scdl", "-l", "u"])
            .unwrap()
            .should_use_tui());
        assert!(Cli::try_parse_from(["scdl", "-l", "u", "--tui"])
            .unwrap()
            .should_use_tui());
        assert!(!Cli::try_parse_from(["scdl", "--no-tui"])
            .unwrap()
            .should_use_tui());
    }

    #[test]
    fn size_parsing_matches_the_documented_suffixes() {
        assert_eq!(parse_size("500k").unwrap(), 512_000);
        assert_eq!(parse_size("5m").unwrap(), 5_242_880);
        assert_eq!(parse_size("1g").unwrap(), 1_073_741_824);
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("1.5m").unwrap(), 1_572_864);
        assert_eq!(parse_size("2M").unwrap(), 2_097_152);
    }

    #[test]
    fn size_parsing_rejects_nonsense() {
        assert!(parse_size("").is_err());
        assert!(parse_size("abc").is_err());
        assert!(parse_size("-5m").is_err());
    }

    #[test]
    fn auth_token_can_come_from_the_environment() {
        // Declared with env = "SCDL_AUTH_TOKEN" so a token need not appear in
        // argv, where `ps` and shell history would capture it.
        let arg = Cli::command()
            .get_arguments()
            .find(|a| a.get_id() == "auth_token")
            .expect("auth_token arg")
            .clone();
        assert!(
            arg.get_env().is_some(),
            "auth-token should accept an env var"
        );
    }
}
